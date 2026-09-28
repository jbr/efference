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
use crate::inference::SessionOptions;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use trillium::Handler;
use trillium_tokio::Swansong;

mod options;
pub use options::SessionOptionsRequest;
use options::preview_session;

mod summary;
pub use summary::SessionSummary;
use summary::summary_stream;

/// One process's sessions and the `Swansong` they all hang under.
pub struct Host {
    swansong: Swansong,
    inference: crate::inference::InferenceRegistry,
    sessions: Mutex<BTreeMap<String, Hosted>>,
    /// Signalled when the session listing may have moved (`summary.rs`): a
    /// session arriving or leaving, and every wake of a session's status
    /// watcher. Every listing subscriber recomputes and diffs for itself, so
    /// this carries no value and an extra ping costs a comparison.
    summary_changed: Notify,
}

/// What the host keeps per session: the state handlers read, the child
/// `Swansong` that ends it, and its `/files` view rooted at its own cwd.
#[derive(Clone)]
pub(super) struct Hosted {
    session: Arc<Session>,
    swansong: Swansong,
    files: Files,
    /// The session that commissioned this one, when a session did (docs/tools.md).
    /// Control state only: never logged, never projected.
    pub(super) created_by: Option<String>,
    /// Whether this session outlives its creator (docs/tools.md). Children end
    /// with the session that made them unless created `detached`.
    pub(super) detached: bool,
    /// The session that handed its work to this one (docs/tools.md) — the one
    /// session outside its descendants this one may archive.
    pub(super) succeeds: Option<String>,
    /// Who hears this session's status, and on what terms (`subscriptions.rs`).
    pub(super) subscribers: BTreeMap<String, super::subscriptions::Subscription>,
    /// The inference options this session was created with — model, spotter,
    /// triage, effort, temperature — which a `hand_off` successor is created
    /// with in turn (docs/tools.md). Empty for a session adopted without a
    /// creation request.
    pub(super) options: crate::inference::SessionOptions,
}

impl Host {
    pub fn new(inference: crate::inference::InferenceRegistry) -> Arc<Self> {
        Arc::new(Self {
            swansong: Swansong::new(),
            inference,
            sessions: Mutex::new(BTreeMap::new()),
            summary_changed: Notify::new(),
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
        self.adopt_with(id, runtime, None, false);
    }

    /// [`adopt`](Self::adopt) recording who commissioned the session
    /// (docs/tools.md): `Some(caller)` for a session-driven `create`, `None`
    /// for a human root. A creator is subscribed to the session's status with
    /// `wake`, and the session ends with its creator unless `detached`.
    pub fn adopt_with(
        self: &Arc<Self>,
        id: String,
        runtime: SessionRuntime,
        created_by: Option<String>,
        detached: bool,
    ) {
        let session = Arc::clone(runtime.session());
        let hosted = Hosted {
            files: Files::rooted_at(session.cwd()),
            session: Arc::clone(&session),
            swansong: runtime.swansong().clone(),
            created_by: created_by.clone(),
            detached,
            succeeds: None,
            subscribers: BTreeMap::new(),
            options: crate::inference::SessionOptions::default(),
        };
        self.sessions.lock().unwrap().insert(id.clone(), hosted);
        self.touch_summary();
        if let Some(creator) = created_by {
            // The creating session is this one's upstream reviewer (docs/permissions.md):
            // the link is recorded first, so the dial below has something to resolve
            // against. A session-created session defaults to `upstream`; an explicit
            // `--autonomous` at creation has already moved the dial off the supervised
            // default and keeps it.
            session.set_upstream(self, creator.clone());
            if session.review_level() == crate::policy::ReviewLevel::SpotterHuman {
                session.set_review_level(crate::policy::ReviewLevel::Upstream);
            }
            self.subscribe(&id, &creator, true);
        }
        // The status watcher (`subscriptions.rs`), stopped by the runtime's
        // shutdown like its other tasks.
        tokio::spawn(
            runtime
                .swansong()
                .interrupt(super::subscriptions::watch_status(
                    Arc::downgrade(self),
                    id.clone(),
                    Arc::clone(&session),
                )),
        );
        let host = Arc::clone(self);
        tokio::spawn(async move {
            let status = match runtime.finished().await {
                Ok(()) => {
                    eprintln!("session {id}: ended");
                    super::status::SessionStatus::Ended
                }
                Err(e) => {
                    eprintln!("session {id}: ended with error: {e}");
                    super::status::SessionStatus::Failed {
                        error: e.to_string(),
                    }
                }
            };
            // The end is the last status the subscribers hear; then the
            // session's own subscriptions and its non-detached children go
            // with it (docs/tools.md).
            host.post_status(&id, &session, &status);
            host.drop_subscriptions_of(&id);
            for child in host.cascade_of(&id) {
                if let Some(done) = host.end(&child) {
                    done.await;
                }
            }
            host.sessions.lock().unwrap().remove(&id);
            host.touch_summary();
            // A surviving child's `upstream` position now has nothing to resolve to, so
            // it reviews as `spotter_human` from here (docs/permissions.md). The
            // resolution is read fresh at every routing, so nothing has to be rewritten
            // — but a child already holding a gate has to be re-routed to the operator,
            // and a state readout has to re-emit, so wake both.
            for child in host.children_of(&id) {
                if let Some(session) = host.session(&child) {
                    session.touch_state();
                }
            }
        });
    }

    /// The session map, locked — for the subscription table (`subscriptions.rs`).
    pub(super) fn sessions_mut(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Hosted>> {
        self.sessions.lock().unwrap()
    }

    /// The session `id` succeeds, when one handed off to it (docs/tools.md).
    pub fn predecessor(&self, id: &str) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .and_then(|hosted| hosted.succeeds.clone())
    }

    /// The session that commissioned `id`, when a session did (docs/tools.md).
    /// Control state — the creation tree, never logged or projected.
    pub fn created_by(&self, id: &str) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .and_then(|hosted| hosted.created_by.clone())
    }
    /// Create a session on this host: the path behind `POST /sessions`
    /// (docs/control-api.md) and the in-session `sessions__create` tool
    /// (docs/tools.md). `lineage` names the calling session as creator when a
    /// session commissioned this one, and the predecessor for a `hand_off`;
    /// default for a human root. Returns the minted id, or the reason nothing was
    /// minted.
    pub async fn create(
        self: &Arc<Self>,
        body: CreateSession,
        lineage: crate::env::Lineage,
    ) -> std::result::Result<String, String> {
        // The session base is what an unnamed id must be free in, on disk and in
        // the live map — a dead session's directory still holds its name
        // (docs/session-title.md).
        let base = body.cwd.join(SESSION_BASE);
        let id = body.name.unwrap_or_else(|| {
            slug::mint(|id| self.session(id).is_some() || base.join(id).exists())
        });
        if !super::tcp::plain_id(&id) {
            return Err(format!(
                "session name {id:?} is not a single plain path segment"
            ));
        }
        if self.session(&id).is_some() {
            return Err(format!("session {id:?} is already hosted"));
        }
        if !body.cwd.is_absolute() {
            return Err(format!("cwd must be absolute: {}", body.cwd.display()));
        }
        let base_env = match crate::env_table::EnvSnapshot::new(body.env) {
            Ok(base) => base,
            Err(e) => return Err(format!("env: {e}")),
        };
        // The same resolution preview reports, then credentials for the roles it
        // selected. Project effort is boot's to apply, so it is not resolved here.
        let options = body.options.clone();
        let resolved = self.inference.resolve_options(&body.options, None);
        let inference = match self.inference.prepare(resolved) {
            Ok(inference) => inference,
            Err(error) => return Err(error.to_string()),
        };
        let request = crate::boot::SessionRequest {
            session_dir: body.cwd.join(SESSION_BASE).join(&id),
            cwd: body.cwd,
            base_env,
            config: body.config,
            inference,
            temperature: body.options.temperature,
            effort: body.options.effort,
            upstream: body.upstream,
            autonomous: body.autonomous,
            reauth: body.reauth,
            keep_scratch: body.keep_scratch,
            // The new session's toolbox mounts the `sessions` provider against
            // this host, with the new session itself as the caller (docs/tools.md).
            host: Some(Arc::clone(self)),
            // The creator and predecessor reach the identity line
            // (docs/context.md); `adopt_with` below records the same values as
            // control state.
            lineage: lineage.clone(),
        };
        let boot = match crate::boot::prepare(request).await {
            Ok(boot) => boot,
            Err(e) => return Err(format!("could not prepare session: {e}")),
        };
        let runtime = SessionRuntime::start(boot, self.swansong());
        self.adopt_with(id.clone(), runtime, lineage.created_by, body.detached);
        self.record_options(&id, options);
        Ok(id)
    }

    /// Hand `from`'s work to a new session (docs/tools.md): one in the same
    /// directory, placed where `from` sits rather than under it — its creator is
    /// `from`'s creator and it outlives `from` exactly as `from` outlived its
    /// own — whose first turn is `brief`, attributed to `from`. `from` records the
    /// successor, which attached clients follow. Returns the successor's id.
    ///
    /// The successor is created with `from`'s own inference options, whatever
    /// `body` carries: a supervisor started on a large model must not hand its
    /// place to whatever the host defaults to, or everything it supervises
    /// starts answering to a weaker model.
    pub async fn hand_off(
        self: &Arc<Self>,
        from: &str,
        mut body: CreateSession,
        brief: String,
    ) -> std::result::Result<String, String> {
        let Some(hosted) = self.hosted(from) else {
            return Err(format!("session {from:?} is not hosted"));
        };
        body.detached = hosted.detached;
        body.options = hosted.options.clone();
        let lineage = crate::env::Lineage {
            created_by: hosted.created_by.clone(),
            succeeds: Some(from.to_string()),
        };
        let id = self.create(body, lineage).await?;
        self.record_succession(from, &id, brief);
        Ok(id)
    }

    /// Record the inference options `id` was created with, which a `hand_off`
    /// successor is created with in turn.
    pub(super) fn record_options(&self, id: &str, options: crate::inference::SessionOptions) {
        if let Some(hosted) = self.sessions.lock().unwrap().get_mut(id) {
            hosted.options = options;
        }
    }

    /// The succession half of [`hand_off`](Self::hand_off), once the successor
    /// exists: it may archive `from`, its first turn is `brief` attributed to
    /// `from`, and `from` names it for the clients to follow.
    pub(crate) fn record_succession(&self, from: &str, successor: &str, brief: String) {
        if let Some(hosted) = self.sessions.lock().unwrap().get_mut(successor) {
            hosted.succeeds = Some(from.to_string());
        }
        if let Some(session) = self.session(successor) {
            session.submit_turn(crate::frontend::SubmittedTurn {
                text: brief,
                submission_id: super::mint_submission_id(),
                source: Some(crate::frontend::handoff_source(from)),
            });
        }
        if let Some(predecessor) = self.session(from) {
            predecessor.set_successor(successor);
        }
        self.touch_summary();
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
        .get("/inference", inference_inventory)
        .with_route(trillium::Method::Query, "/sessions", preview_session)
        .get("/sessions", list_sessions())
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

/// `GET /inference`: configured host connections, sorted by name. The registry is
/// loaded once at host startup, so this never reads config or credentials per request.
async fn inference_inventory(conn: Conn) -> Conn {
    match host_of(&conn) {
        Some(host) => conn
            .with_status(200)
            .with_body(serde_json::json!({ "connections": host.inference.inventory() }).to_string())
            .halt(),
        None => missing_host(conn),
    }
}

/// `GET /sessions`: the listing (`summary.rs`) — one row per hosted session,
/// sorted by id.
///
/// The route is a tuple so the same URL serves both readings of the one
/// resource. The `Sse` half negotiates on `Accept`: a request naming
/// `text/event-stream` subscribes to the listing and receives it whole on every
/// change, and any other request passes through untouched to the JSON half
/// below (docs/control-api.md).
fn list_sessions() -> impl trillium::Handler {
    (
        sse(|conn: &mut Conn| match host_of(conn) {
            Some(host) => summary_stream(Arc::downgrade(&host)),
            // Unreachable through the mounted router, which always carries the
            // host as shared state; an empty stream closes rather than hangs.
            None => Box::pin(futures_lite::stream::empty()),
        })
        .with_heartbeat(super::http::EVENTS_HEARTBEAT),
        read_sessions,
    )
}

/// The JSON half of [`list_sessions`].
async fn read_sessions(conn: Conn) -> Conn {
    match host_of(&conn) {
        Some(host) => conn
            .with_status(200)
            .with_body(host.summaries_json())
            .halt(),
        None => missing_host(conn),
    }
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
    /// The session id; a minted adjective-noun pair when absent
    /// (docs/session-title.md). A single plain path segment, as the gate's
    /// `plain_id` requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The session's working root. Absolute, always sent.
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    /// The editable selections, spelled exactly as `QUERY /sessions` accepts
    /// them and resolved by the same flow (docs/control-api.md). Flattened, so
    /// the two routes cannot drift and a new option is one field in one place.
    #[serde(flatten)]
    pub options: SessionOptions,
    #[serde(default)]
    pub upstream: Vec<String>,
    #[serde(default)]
    pub autonomous: bool,
    #[serde(default)]
    pub reauth: Vec<String>,
    #[serde(default)]
    pub keep_scratch: bool,
    /// Outlive the session that creates this one (docs/tools.md). Meaningful only
    /// for a session-driven `create`: a human root has no creator to end with.
    #[serde(default)]
    pub detached: bool,
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
        Err(error) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("invalid body: {error}"),
            );
        }
    };
    // A human root: no session commissioned it, so it has no `created_by`
    // (docs/tools.md).
    match host.create(body, crate::env::Lineage::default()).await {
        Ok(id) => conn
            .with_status(201)
            .with_body(serde_json::json!({ "id": id }).to_string())
            .halt(),
        Err(reason) => fail(conn, ErrorCode::BadRequest, reason),
    }
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
        // Bound before the link is minted: a bind to port 0 learns its port here.
        if let Some(bound) = handle.info().await.tcp_socket_addr() {
            auth.bound_port(bound.port());
        }
        eprintln!("web: {} (bound {addr})", auth.public_base());
        let link = auth.mint_link();
        match &link {
            Some(link) => eprintln!("  {link}"),
            None => {
                eprintln!("  (every current code is used; a link minted in half a minute works)")
            }
        }
        if let Some(link) = link.filter(|_| open)
            && webbrowser::open(&link).is_err()
        {
            eprintln!("[could not open a browser for {link}]");
        }
        _tcp_server = Some(handle);
    }
    // The listener's shutdown completes only when every session's teardown
    // guard has dropped: the runtimes are children of the same `Swansong`.
    server.await;
    Ok(())
}
