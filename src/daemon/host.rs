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

mod create;
pub use create::CreateSession;
use create::create_session;

mod options;
pub use options::SessionOptionsRequest;
use options::preview_session;

mod resume;
use resume::patch_session;
pub use resume::{PatchSession, SessionState};

mod archived;
pub use archived::ArchivedSession;
use archived::{archived_listing, read_session};

mod catalog;
use catalog::{catalog_all, catalog_one};

mod projects;
use projects::{forget_project, list_projects};

mod listing;
pub use listing::ListedSession;
use listing::listing_stream;

mod supervise;
pub use supervise::SupervisorRefused;

/// One process's sessions and the `Swansong` they all hang under.
pub struct Host {
    swansong: Swansong,
    inference: crate::inference::InferenceRegistry,
    /// Each connection's model listing, stored in the user state database
    /// (docs/model-catalog.md).
    catalog: crate::catalog::Catalog,
    /// Where sessions have been launched, in the same database (docs/control-api.md).
    /// Written by every successful creation and read by the index route.
    remembered: crate::remembered::Projects,
    sessions: Mutex<BTreeMap<String, Hosted>>,
    /// Scanned rows of the dormant sessions under each root asked about
    /// (`archived.rs`, docs/log-and-projection.md).
    inventory: crate::inventory::Inventory,
    /// The host's own environment, captured once at start (docs/env.md).
    ///
    /// This is the base a session launched without a caller-supplied environment
    /// inherits, and what an `env_hook` inherits itself. It is taken once, at the
    /// same moment the registry resolved this host's credentials from it, so the
    /// baseline a launch starts from is a stated thing rather than whatever the
    /// process happens to be carrying at request time. `EnvSnapshot` omits Debug
    /// and Serialize, so it cannot be logged or echoed, and the credential
    /// variables in it are stripped and masked for every session by the same
    /// [`crate::env_table::EnvPolicy::build_session`] call as any other base
    /// (docs/env.md).
    host_env: crate::env_table::EnvSnapshot,
    /// Signalled when the session listing may have moved (`listing.rs`): a
    /// session arriving or leaving, and every wake of a session's status
    /// watcher. Every listing subscriber recomputes and diffs for itself, so
    /// this carries no value and an extra ping costs a comparison.
    listing_changed: Notify,
    /// What every client has last said about whether anyone is attending
    /// (`attention.rs`, docs/web-push.md). Shared with each adopted session, so
    /// a push for any of them reads one table.
    pub(super) attention: Arc<super::attention::Attention>,
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
    /// The session that supervises this one (docs/permissions.md): it answers this
    /// one's gates under the `supervisor` review position and may drive it. Starts as
    /// `created_by`, and moves only by `Host::set_supervisor` (`supervise.rs`).
    /// Control state only: not persisted, so a resume starts again from the creator.
    pub(super) supervisor: Option<String>,
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
    /// `host_env` is the host's own environment at start, captured by the caller
    /// that loaded the inference registry from it (docs/env.md).
    pub fn new(
        inference: crate::inference::InferenceRegistry,
        host_env: crate::env_table::EnvSnapshot,
    ) -> Arc<Self> {
        let catalog =
            crate::catalog::Catalog::new(inference.history().state_path().map(ToOwned::to_owned));
        let inference = inference.with_catalog(catalog.clone());
        Arc::new(Self {
            swansong: Swansong::new(),
            catalog,
            // The same file as the catalog and the picks, and disabled with them:
            // a config that named no home directory names no place to remember.
            remembered: inference
                .history()
                .state_path()
                .map_or_else(crate::remembered::Projects::disabled, |path| {
                    crate::remembered::Projects::at(path.to_owned())
                }),
            inference,
            sessions: Mutex::new(BTreeMap::new()),
            inventory: crate::inventory::Inventory::new(),
            host_env,
            listing_changed: Notify::new(),
            attention: Arc::default(),
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
    pub fn adopt(self: &Arc<Self>, id: String, runtime: SessionRuntime) -> Result<(), String> {
        self.adopt_with(id, runtime, None, false)
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
    ) -> Result<(), String> {
        let session = Arc::clone(runtime.session());
        // The web client addresses an adopted session under the host's own
        // mount, and a push notification opens that path (docs/web-push.md).
        session.set_web_path(format!("/sessions/{id}/"));
        session.set_attention(Arc::clone(&self.attention));
        let hosted = Hosted {
            files: Files::rooted_at(session.cwd()),
            session: Arc::clone(&session),
            swansong: runtime.swansong().clone(),
            created_by: created_by.clone(),
            supervisor: created_by.clone(),
            detached,
            succeeds: None,
            subscribers: BTreeMap::new(),
            options: crate::inference::SessionOptions::default(),
        };
        {
            let mut sessions = self.sessions.lock().unwrap();
            // First one in wins. A create checks this id is free *before* it awaits
            // the hook and prepares, so two creates naming the same id in different
            // directories both pass that check and arrive here together; the loser's
            // insert would otherwise replace the winner's entry and leave a running
            // session unreachable.
            if sessions.contains_key(&id) {
                // This runtime is live and will never be in the map, so it is shut
                // down here rather than left running unhosted.
                runtime.swansong().shut_down();
                return Err(format!("session {id:?} is already hosted"));
            }
            sessions.insert(id.clone(), hosted);
        }
        self.touch_listing();
        // The link to the supervisor pointer is made for every adopted session, root or
        // not, since the pointer can be set after creation (docs/permissions.md).
        session.link_supervisor(self, id.clone());
        if let Some(creator) = created_by {
            // The creating session is this one's supervisor by default: the pointer
            // above already names it, so the dial below has something to resolve
            // against. A session-created session defaults to `supervisor`; an explicit
            // `--autonomous` at creation has already moved the dial off the supervised
            // default and keeps it.
            session.default_to_supervisor();
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
                    log::info!("session {id}: ended");
                    super::status::SessionStatus::Ended
                }
                Err(e) => {
                    log::warn!("session {id}: ended with error: {e}");
                    super::status::SessionStatus::Failed {
                        error: e.to_string(),
                    }
                }
            };
            // The end is the last status the subscribers hear; then the
            // session's own subscriptions and its non-detached children go
            // with it (docs/tools.md).
            // An archived session has nothing left to announce.
            session.cancel_escalation(crate::log::PushCancel::Archived);
            host.post_status(&id, &session, &status);
            host.drop_subscriptions_of(&id);
            for child in host.cascade_of(&id) {
                if let Some(done) = host.end(&child) {
                    done.await;
                }
            }
            host.sessions.lock().unwrap().remove(&id);
            host.touch_listing();
            // A session this one supervised now has nothing to resolve its `supervisor`
            // position to, so it reviews as `spotter_human` from here
            // (docs/permissions.md). The resolution is read fresh at every routing, so
            // nothing has to be rewritten — but a session already holding a gate has to
            // be re-routed to the operator, and a state readout has to re-emit, so wake
            // both.
            for supervised in host.supervised_by(&id) {
                if let Some(session) = host.session(&supervised) {
                    session.touch_state();
                }
            }
        });
        Ok(())
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
            session.submit_turn(
                crate::frontend::SubmittedTurn::new(brief, super::mint_submission_id())
                    .sourced(Some(crate::frontend::handoff_source(from))),
            );
        }
        if let Some(predecessor) = self.session(from) {
            predecessor.set_successor(successor);
        }
        self.touch_listing();
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

    /// Archive one session: shut it down, then retire its own worktree when it made
    /// one (docs/control-api.md). Resolves when teardown has run, with what became of
    /// the worktree (`None` when the session had none, or its directory was already
    /// gone). `None` when no session has that id. The map entry is removed by the
    /// watcher [`adopt`](Self::adopt) spawned, once the loop has returned.
    ///
    /// This is the archive path only. A host shutdown stops sessions without it and
    /// leaves every worktree in place, including through the cascade that ends a
    /// stopped session's children: those reach here with the host already shutting
    /// down, and retire nothing.
    pub fn end(
        &self,
        id: &str,
    ) -> Option<impl std::future::Future<Output = Option<crate::worktree::Retired>>> {
        let hosted = self.hosted(id)?;
        let id = id.to_string();
        let archiving = self.swansong.state().is_running();
        Some(async move {
            hosted.swansong.shut_down().await;
            let retired = if archiving {
                retire_worktree(&hosted.session, &id).await
            } else {
                None
            };
            // The retirement above is the last append. Closing here rather than at
            // the `Session`'s drop is what lets a resume follow the archive at once:
            // an open SSE stream or a handler can still be holding the session.
            hosted.session.close_log();
            // Announced only for a deliberate archive: a host shutdown cascades
            // sessions through here with `archiving` false, and a dying host has
            // nothing to announce to.
            if archiving {
                log::info!("session {id}: archived");
            }
            retired
        })
    }
}

/// Retire the worktree `session` made for itself (docs/worktrees.md), if it did:
/// its working directory is the one [`crate::worktree::path`] names for its id. A
/// session that inherited a worktree works inside one it did not make and leaves it.
/// Git runs on a blocking thread; the outcome is appended to the session's log, which
/// outlives the runtime's shutdown because the `Session` owns it.
async fn retire_worktree(session: &Arc<Session>, id: &str) -> Option<crate::worktree::Retired> {
    let directories = session.directories();
    let project = directories.project().to_path_buf();
    if directories.working() != crate::worktree::path(&project, id) {
        return None;
    }
    let env = session.child_env();
    let (git_project, git_id) = (project.clone(), id.to_string());
    let retired =
        tokio::task::spawn_blocking(move || crate::worktree::retire(&git_project, &git_id, &env))
            .await
            .unwrap_or_else(|e| crate::worktree::Retired::Failed(format!("worktree task: {e}")));
    if let Some(event) = retired.clone().into_event(&project, id)
        && let Err(e) = session.append(0, crate::log::Payload::Worktree(event))
    {
        log::warn!("session {id}: could not log the worktree outcome: {e}");
    }
    (retired != crate::worktree::Retired::Absent).then_some(retired)
}

/// The host's routes (docs/control-api.md). `POST /sessions` is mounted on every
/// host listener, socket and network alike: creation's environment is
/// established per request (docs/env.md), so what it needs is the hook or the
/// host's own baseline rather than a local caller. `files` serves each session's
/// `/files` under its prefix — the web transport, as on a session bind.
pub(crate) fn host_routes(files: bool) -> impl trillium::Handler {
    let router = Router::new()
        .get("/inference", inference_inventory)
        .get("/inference/catalog", catalog_all)
        .get("/inference/catalog/:connection", catalog_one)
        .with_route(trillium::Method::Query, "/sessions", preview_session)
        .get("/sessions", list_sessions())
        // Registered before the `/sessions/:session/*` wildcard below, which would
        // resolve the session into conn state and 404 a patch that is what
        // makes it live in the first place (and a read of a dormant session).
        .get("/sessions/:session", read_session)
        .patch("/sessions/:session", patch_session)
        .delete("/sessions/:session", end_session)
        // Where sessions have been started, and forgetting one. Registered
        // before the `/sessions/:session/*` wildcard below, which is a different
        // prefix but the same reader of a path's first segment.
        .get("/projects", list_projects)
        // Clients report whether a person is attending (`attention.rs`).
        .post("/attention", report_attention)
        .delete("/projects", forget_project)
        // The wildcard remainder becomes the conn's path, so the control routes
        // match `/v1/…` exactly as they do at a session bind's root.
        //
        // The session segment is `:session`, never `:id`: the wildcard rewrites
        // the *path* but leaves this capture in the conn's params, and a
        // same-named capture inside `control_routes` (`/v1/processes/:id`,
        // `/v1/notifications/:id`) would then read the session name where the
        // route means an ordinal — every such route answering 400
        // "process id must be an integer" for a well-formed id.
        .all(
            "/sessions/:session/*",
            (
                move |conn: Conn| async move { resolve_session(conn, files).await },
                control_routes(),
                files.then_some(session_files),
            ),
        );
    router.post("/sessions", create_session)
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

/// `POST /attention`: one client's report of whether a person is attending, and
/// which session it has open (`attention.rs`, docs/web-push.md). `204` on
/// success; a malformed body is a `400` naming the problem.
async fn report_attention(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    match super::attention::AttentionReport::parse(&body) {
        Ok(report) => {
            host.report_attention(&report);
            conn.with_status(204).halt()
        }
        Err(message) => fail(conn, ErrorCode::BadRequest, message),
    }
}

/// `GET /sessions`: the listing (`listing.rs`) — one row per hosted session,
/// sorted by id.
///
/// The route is a tuple so the same URL serves both readings of the one
/// resource. The `Sse` half negotiates on `Accept`: a request naming
/// `text/event-stream` subscribes to the listing and receives it whole on every
/// change, and any other request passes through untouched to the JSON half
/// below (docs/control-api.md). A `state` query is read first (`archived.rs`):
/// `archived` answers the dormant listing as JSON whatever the `Accept`.
fn list_sessions() -> impl trillium::Handler {
    (
        archived_listing,
        sse(|conn: &mut Conn| match host_of(conn) {
            Some(host) => listing_stream(Arc::downgrade(&host)),
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
        Some(host) => conn.with_status(200).with_body(host.listing_json()).halt(),
        None => missing_host(conn),
    }
}

/// The per-session `/files` view, handed from [`resolve_session`] through conn
/// state so one stage serves every session. `Files` is a handle over shared
/// watcher and ignore state, so cloning it per request is cheap.
struct HostedFiles(Files);

/// `/sessions/:session/*`: put the session into conn state for the control
/// routes, or `404` when no hosted session has that id. Exact public web-shell
/// routes pass through even without a session (docs/web-client.md).
async fn resolve_session(conn: Conn, web: bool) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    match host.hosted(&id) {
        Some(hosted) => conn
            .with_state(hosted.session)
            .with_state(HostedFiles(hosted.files)),
        // The router has stripped the session prefix. These exact routes carry
        // only the public shell; the client then observes the API's 404 and goes
        // home (docs/web-client.md). Control and file paths still fail here.
        None if web
            && conn.method() == trillium::Method::Get
            && super::tcp::Surface::Session
                .spa_route(&format!("/{}", conn.path().trim_start_matches('/'))) =>
        {
            conn
        }
        None => no_such_session(conn, &id),
    }
}

async fn session_files(mut conn: Conn) -> Conn {
    match conn.take_state::<HostedFiles>() {
        Some(HostedFiles(files)) => files.handler().run(conn).await,
        None => conn,
    }
}

/// `DELETE /sessions/:session`: archive the session. The same transition as
/// `PATCH {"state": "archived"}`, in the imperative spelling, and answering once
/// teardown has run — background children reaped, scratch removed — so a client
/// that then lists sessions or reads the dir sees the finished state.
///
/// Archiving rather than ending is what it does: the runtime stops, and the
/// directory and log stay, so the session is resumable by patching it `active`
/// again (docs/log-and-projection.md).
async fn end_session(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    match host.end(&id) {
        Some(completion) => {
            let retired = completion.await;
            archived(conn, &id, retired)
        }
        None => no_such_session(conn, &id),
    }
}

/// The `200` an archive answers with, carrying `worktree` when the session had its own
/// (docs/control-api.md).
fn archived(conn: Conn, id: &str, retired: Option<crate::worktree::Retired>) -> Conn {
    let mut body = serde_json::json!({ "id": id, "state": SessionState::Archived });
    if let Some(worktree) = retired.and_then(|retired| retired.response()) {
        body["worktree"] = worktree;
    }
    conn.with_status(200).with_body(body.to_string()).halt()
}

fn no_such_session(conn: Conn, id: &str) -> Conn {
    fail(
        conn,
        ErrorCode::NotFound,
        format!("no hosted session {id:?}; read GET /sessions for the current list"),
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
            App::Host,
        ));
    log::info!("host socket: {}", socket.display());
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
                App::Host,
            ),
            host.swansong().clone(),
            Arc::clone(&host),
        )
        .map_err(|e| -> Error { format!("failed to bind {addr}: {e}").into() })?;
        // Bound before the link is minted: a bind to port 0 learns its port here.
        if let Some(bound) = handle.info().await.tcp_socket_addr() {
            auth.bound_port(bound.port());
        }
        log::info!("web: {} (bound {addr})", auth.public_base());
        let link = auth.mint_link();
        match &link {
            Some(link) => log::info!("  {link}"),
            None => {
                log::info!("  (every current code is used; a link minted in half a minute works)")
            }
        }
        if let Some(link) = link.filter(|_| open)
            && webbrowser::open(&link).is_err()
        {
            log::warn!("could not open a browser for {link}");
        }
        _tcp_server = Some(handle);
    }
    // The listener's shutdown completes only when every session's teardown
    // guard has dropped: the runtimes are children of the same `Swansong`.
    server.await;
    Ok(())
}
