//! The host: one process owning many sessions and serving as their control
//! endpoint (docs/control-channel.md).
//!
//! A [`Host`] holds a map of started [`SessionRuntime`]s under one root `Swansong`.
//! Its routes are the listener's own surface — the session list, creation, ending
//! a session — plus `/sessions/<id>/…`, which resolves the session into conn state
//! and dispatches into the unchanged control routes (docs/control-api.md). Nothing
//! is forwarded: the session's handlers run in this process against its `Session`.

use super::errors::{ErrorCode, fail};
use super::files::Files;
use super::http::control_routes;
use super::runtime::SessionRuntime;
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use trillium::Handler;
use trillium_tokio::Swansong;

/// One process's sessions and the `Swansong` they all hang under.
pub struct Host {
    swansong: Swansong,
    inference: crate::inference::InferenceRegistry,
    sessions: Mutex<BTreeMap<String, Hosted>>,
}

/// What the host keeps per session: the state handlers read, the child
/// `Swansong` that ends it, and its `/files` view rooted at its own cwd.
#[derive(Clone)]
struct Hosted {
    session: Arc<Session>,
    swansong: Swansong,
    files: Files,
}

impl Host {
    pub fn new(inference: crate::inference::InferenceRegistry) -> Arc<Self> {
        Arc::new(Self {
            swansong: Swansong::new(),
            inference,
            sessions: Mutex::new(BTreeMap::new()),
        })
    }

    /// The root every session runtime is a child of. Listeners are configured
    /// with it too, so a signal stops the listeners and, through the child
    /// tree, every session.
    pub fn swansong(&self) -> &Swansong {
        &self.swansong
    }

    /// Register a started runtime under `id` and watch for its end: a session
    /// that finishes on its own — end of input, a failed loop, or its own
    /// `Swansong` — leaves the map when its teardown has run. The runtime must
    /// have been started under [`swansong`](Self::swansong).
    pub fn adopt(self: &Arc<Self>, id: String, runtime: SessionRuntime) {
        let session = Arc::clone(runtime.session());
        let hosted = Hosted {
            files: Files::rooted_at(session.cwd()),
            session,
            swansong: runtime.swansong().clone(),
        };
        self.sessions.lock().unwrap().insert(id.clone(), hosted);
        let host = Arc::clone(self);
        tokio::spawn(async move {
            match runtime.finished().await {
                Ok(()) => eprintln!("session {id}: ended"),
                Err(e) => eprintln!("session {id}: ended with error: {e}"),
            }
            host.sessions.lock().unwrap().remove(&id);
        });
    }

    /// The hosted session ids, sorted (docs/control-api.md).
    pub fn ids(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|h| Arc::clone(&h.session))
    }

    fn hosted(&self, id: &str) -> Option<Hosted> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// Initiate shutdown of one session; resolves when its teardown has run.
    /// `None` when no session has that id. The map entry is removed by the
    /// watcher [`adopt`](Self::adopt) spawned, once the loop has returned.
    pub fn end(&self, id: &str) -> Option<impl std::future::Future<Output = ()>> {
        self.hosted(id).map(|h| h.swansong.shut_down())
    }
}

/// The host's routes (docs/control-api.md). `create` mounts `POST /sessions`;
/// a listener that does not carry it answers the same path with a refusal, so
/// the endpoint is one and its availability is a property of the listener the
/// request arrived on, not a runtime guess. `files` serves each session's
/// `/files` under its prefix — the web transport, as on a session bind.
pub(crate) fn host_routes(create: bool, files: bool) -> impl trillium::Handler {
    let mut router = Router::new()
        .get("/sessions", list_sessions)
        .delete("/sessions/:id", end_session)
        // The wildcard remainder becomes the conn's path, so the control routes
        // match `/v1/…` exactly as they do at a session bind's root.
        .all(
            "/sessions/:id/*",
            (
                resolve_session,
                control_routes(),
                files.then_some(session_files),
            ),
        );
    router = if create {
        router.post("/sessions", create_session)
    } else {
        router.post("/sessions", refuse_remote_create)
    };
    router
}

fn host_of(conn: &Conn) -> Option<Arc<Host>> {
    conn.shared_state::<Arc<Host>>().map(Arc::clone)
}

fn missing_host(conn: Conn) -> Conn {
    fail(conn, ErrorCode::Internal, "missing host state")
}

/// `GET /sessions`: the hosted ids, as `[{"id": …}, …]`, sorted.
async fn list_sessions(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let ids: Vec<_> = host
        .ids()
        .into_iter()
        .map(|id| serde_json::json!({ "id": id }))
        .collect();
    conn.with_status(200)
        .with_body(serde_json::Value::Array(ids).to_string())
        .halt()
}

/// The per-session `/files` view, handed from [`resolve_session`] through conn
/// state so one stage serves every session. `Files` is a handle over shared
/// watcher and ignore state, so cloning it per request is cheap.
struct HostedFiles(Files);

/// `/sessions/:id/*`: put the session into conn state for the control routes,
/// or `404` when no hosted session has that id.
async fn resolve_session(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("id").unwrap_or_default().to_string();
    match host.hosted(&id) {
        Some(hosted) => conn
            .with_state(hosted.session)
            .with_state(HostedFiles(hosted.files)),
        None => no_such_session(conn, &id),
    }
}

async fn session_files(mut conn: Conn) -> Conn {
    match conn.take_state::<HostedFiles>() {
        Some(HostedFiles(files)) => files.handler().run(conn).await,
        None => conn,
    }
}

/// `DELETE /sessions/:id`: end the session. Answers once teardown has run —
/// background children reaped, scratch removed — so a client that then lists
/// sessions or reads the dir sees the finished state.
async fn end_session(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("id").unwrap_or_default().to_string();
    match host.end(&id) {
        Some(completion) => {
            completion.await;
            conn.with_status(200)
                .with_body(serde_json::json!({ "id": id, "state": "ended" }).to_string())
                .halt()
        }
        None => no_such_session(conn, &id),
    }
}

fn no_such_session(conn: Conn, id: &str) -> Conn {
    fail(
        conn,
        ErrorCode::NotFound,
        format!("no hosted session {id:?}; read GET /sessions for the current list"),
    )
}

/// Body of `POST /sessions` (docs/control-api.md): a `SessionRequest` as a
/// client spells it. One type on both ends — the control client serializes it,
/// the host deserializes it — so a new session knob is one field here. `env` is
/// the session's complete base environment (docs/env.md); it is applied and
/// dropped, never logged or echoed, which is why the type has no Debug.
#[derive(Serialize, Deserialize)]
pub struct CreateSession {
    /// The session id; a generated timestamp when absent. A single plain path
    /// segment, as the gate's `plain_id` requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The session's working root. Absolute, always sent.
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<crate::distill::TriageModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub upstream: Vec<String>,
    #[serde(default)]
    pub autonomous: bool,
    #[serde(default)]
    pub reauth: Vec<String>,
    #[serde(default)]
    pub keep_scratch: bool,
    pub env: BTreeMap<String, String>,
}

/// `POST /sessions` on the Unix socket: prepare and start a session from the
/// body. Preparation reports to this process's stderr and may open a browser
/// for an MCP authorization, on the host (backlog/host-process.md).
async fn create_session(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: CreateSession = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(e) => return fail(conn, ErrorCode::BadRequest, format!("invalid body: {e}")),
    };
    let id = body.name.unwrap_or_else(default_session_id);
    if !super::tcp::plain_id(&id) {
        return fail(
            conn,
            ErrorCode::BadRequest,
            format!("session name {id:?} is not a single plain path segment"),
        );
    }
    if host.session(&id).is_some() {
        return fail(
            conn,
            ErrorCode::BadRequest,
            format!("session {id:?} is already hosted"),
        );
    }
    if !body.cwd.is_absolute() {
        return fail(
            conn,
            ErrorCode::BadRequest,
            format!("cwd must be absolute: {}", body.cwd.display()),
        );
    }
    let base_env = match crate::env_table::EnvSnapshot::new(body.env) {
        Ok(base) => base,
        Err(e) => return fail(conn, ErrorCode::BadRequest, format!("env: {e}")),
    };
    let inference = match host.inference.resolve(
        body.model.as_deref(),
        body.spotter_model.as_deref(),
        body.triage,
    ) {
        Ok(inference) => inference,
        Err(error) => return fail(conn, ErrorCode::BadRequest, error.to_string()),
    };
    let request = crate::boot::SessionRequest {
        session_dir: body.cwd.join(SESSION_BASE).join(&id),
        cwd: body.cwd,
        base_env,
        config: body.config,
        inference,
        temperature: body.temperature,
        effort: body.effort,
        upstream: body.upstream,
        autonomous: body.autonomous,
        reauth: body.reauth,
        keep_scratch: body.keep_scratch,
    };
    let boot = match crate::boot::prepare(request).await {
        Ok(boot) => boot,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("could not prepare session: {e}"),
            );
        }
    };
    let runtime = SessionRuntime::start(boot, host.swansong());
    host.adopt(id.clone(), runtime);
    conn.with_status(201)
        .with_body(serde_json::json!({ "id": id }).to_string())
        .halt()
}

/// `POST /sessions` on a network listener: remote creation needs an environment
/// recipe the host does not have yet (backlog/host-process.md).
async fn refuse_remote_create(conn: Conn) -> Conn {
    fail(
        conn,
        ErrorCode::Forbidden,
        "session creation is accepted on the host's Unix socket only",
    )
}

/// A session id when none was named: timestamp millis.
fn default_session_id() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis()
        .to_string()
}

/// Run a host: bind the control socket and the optional TCP listener over
/// `host`, and serve until shutdown. Sessions arrive by `POST /sessions`.
pub async fn serve_host(
    host: Arc<Host>,
    socket: PathBuf,
    bind: Option<tcp::TcpBind>,
) -> Result<(), Error> {
    let web = bind.map(tcp::arm);
    let web_auth = web.as_ref().map(|(auth, _)| Arc::clone(auth));
    let server = trillium_tokio::config()
        .with_swansong(host.swansong().clone())
        .with_shared_state(Arc::clone(&host))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(handler(
            Transport::Socket {
                auth: web_auth.clone(),
            },
            App::Host { create: true },
        ));
    eprintln!("host socket: {}", socket.display());
    let mut _tcp_server = None;
    if let Some((auth, listener)) = web {
        let open = listener.open;
        let addr = listener.addr.clone();
        let handle = tcp::spawn(
            listener,
            handler(
                Transport::Web {
                    auth: Arc::clone(&auth),
                },
                App::Host { create: false },
            ),
            host.swansong().clone(),
            Arc::clone(&host),
        )
        .map_err(|e| -> Error { format!("failed to bind {addr}: {e}").into() })?;
        let link = auth.mint_link();
        eprintln!("web: {} (bound {addr})", auth.public_base());
        eprintln!("  {link}");
        handle.info().await;
        if open && webbrowser::open(&link).is_err() {
            eprintln!("[could not open a browser for {link}]");
        }
        _tcp_server = Some(handle);
    }
    // The listener's shutdown completes only when every session's teardown
    // guard has dropped: the runtimes are children of the same `Swansong`.
    server.await;
    Ok(())
}
