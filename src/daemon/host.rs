//! The host: one process owning many sessions and serving as their control
//! endpoint (docs/control-channel.md).
//!
//! A [`Host`] holds a map of sessions under one root `Swansong`: each one starting
//! while its environment and MCP mounts are prepared (`entries.rs`,
//! `preparation.rs`), then a started [`SessionRuntime`]. Its routes are the
//! listener's own surface — the session list, creation, ending a session — plus
//! `/sessions/<id>/…`, which resolves the session into conn state and dispatches into
//! the unchanged control routes (docs/control-api.md). Nothing is forwarded: the
//! session's handlers run in this process against its `Session`.

use super::errors::{ErrorCode, fail};
use super::files::Files;
use super::http::control_routes;
use super::permissions;
use super::runtime::SessionRuntime;
use super::*;
use crate::inference::SessionOptions;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
pub use archived::{ArchivedSession, DormantSession};
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

mod entries;
use entries::{Entry, Sessions};

mod preparation;

mod boot_resume;

mod end;
pub use end::Ended;

mod island_exit;

mod agents;

mod update_check;

mod routes;
pub(crate) use routes::host_routes;
use routes::{host_of, missing_host, no_such_session};

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
    /// The host-wide register of session ids, in the same database
    /// (`session_ids.rs`, docs/session-title.md).
    session_ids: crate::session_ids::SessionIds,
    /// Every session on this host, starting or hosted (`entries.rs`).
    sessions: Mutex<Sessions>,
    /// Starting sessions whose preparation failed, kept for `FAILURE_KEPT` after
    /// they left the map so a client that attaches after the failure still reads it
    /// (`preparation.rs`). Served on `/state` and `/events` only; neither listed nor
    /// holding an id.
    failed: Mutex<BTreeMap<String, (Arc<starting::Starting>, Instant)>>,
    /// Why the last resume of a dormant session failed, by project root and id, until
    /// a resume succeeds or the session is archived. The dormant listing carries it as
    /// `resume_failure` (`archived.rs`).
    resume_failures: Mutex<BTreeMap<(PathBuf, String), String>>,
    /// Scanned rows of the dormant sessions under each root asked about
    /// (`archived.rs`, docs/log-and-projection.md).
    inventory: crate::inventory::Inventory,
    /// Turns `true` when the host-start pass (the id backfill) has run; the boot
    /// resume waits for it (`boot_resume.rs`).
    start_pass: tokio::sync::watch::Receiver<bool>,
    /// Set when an owner watch adopts this host, which makes it an island
    /// (`island_exit.rs`, docs/operating.md): it runs no boot resume, and it
    /// archives its sessions when its owner goes (docs/resumption.md).
    island: AtomicBool,
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
    pub(super) attention: Arc<attention::Attention>,
    /// What the update check last found newer than this host (`update_check.rs`,
    /// docs/update-check.md). Shared with each adopted session, which publishes it as
    /// its `newer_version` topic; empty on a host that does not check.
    newer_version: Arc<newer_version::NewerVersion>,
    /// The artifact-only loopback listener's origin (docs/artifacts.md): started by
    /// the first adopt, under this host's `Swansong`; `None` once its bind failed, so
    /// the bind is tried once.
    artifact_origin: std::sync::OnceLock<Option<String>>,
    /// One lock per MCP server for refreshing its stored grant, shared by the token
    /// store every session here mounts with, so the host's sessions refresh a
    /// server's grant once however many find it expired (docs/mcp-auth.md).
    mcp_refresh_locks: crate::mcp::auth::store::RefreshLocks,
    /// The language servers behind the `lsp` built-ins (docs/tools.md). Workspaces are
    /// keyed by project root, so the sessions in one project share one set of servers;
    /// each session's binding releases its interest when its toolbox drops, and
    /// [`serve_host`] stops what remains after the sessions have ended.
    language_servers: Arc<crate::tools::lsp::LanguageServers>,
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
    pub(super) subscribers: BTreeMap<String, subscriptions::Subscription>,
    /// The last seq each reading session was shown of this one's transcript
    /// (`agents.rs`, docs/tools.md). Control state only: never logged, gone with a
    /// restart, when a reader's next default read starts over at the tail.
    pub(super) read_cursors: BTreeMap<String, Seq>,
    /// The inference options this session was created with — model, spotter,
    /// triage, effort, temperature — which a `hand_off` successor is created
    /// with in turn (docs/tools.md). Empty for a session adopted without a
    /// creation request.
    pub(super) options: SessionOptions,
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
        let session_ids = inference
            .history()
            .state_path()
            .map_or_else(crate::session_ids::SessionIds::disabled, |path| {
                crate::session_ids::SessionIds::at(path.to_owned())
            });
        let remembered = inference
            .history()
            .state_path()
            .map_or_else(crate::remembered::Projects::disabled, |path| {
                crate::remembered::Projects::at(path.to_owned())
            });
        let start_pass = Self::backfill_session_ids(&session_ids, &remembered);
        Arc::new(Self {
            swansong: Swansong::new(),
            catalog,
            // The same file as the catalog and the picks, and disabled with them:
            // a config that named no home directory names no place to remember.
            remembered,
            session_ids,
            inference,
            sessions: Mutex::new(Sessions::default()),
            failed: Mutex::new(BTreeMap::new()),
            resume_failures: Mutex::new(BTreeMap::new()),
            inventory: crate::inventory::Inventory::new(),
            start_pass,
            island: AtomicBool::new(false),
            host_env,
            listing_changed: Notify::new(),
            attention: Arc::default(),
            newer_version: Arc::default(),
            artifact_origin: std::sync::OnceLock::new(),
            mcp_refresh_locks: Default::default(),
            language_servers: crate::tools::lsp::language_servers(),
        })
    }

    /// The language servers behind the `lsp` built-ins, shared by every session on this
    /// host (docs/tools.md).
    pub(crate) fn language_servers(&self) -> &Arc<crate::tools::lsp::LanguageServers> {
        &self.language_servers
    }

    /// The MCP refresh locks a session's token store shares (docs/mcp-auth.md).
    pub(crate) fn mcp_refresh_locks(&self) -> crate::mcp::auth::store::RefreshLocks {
        self.mcp_refresh_locks.clone()
    }

    /// The artifact-only loopback listener's origin, starting the listener on first
    /// use (docs/artifacts.md). It serves only artifact routes, so it is started for
    /// any host, with or without a TCP bind.
    pub(crate) fn artifact_origin(self: &Arc<Self>) -> Option<&str> {
        self.artifact_origin
            .get_or_init(|| match artifacts::spawn_listener(self) {
                Ok(origin) => {
                    log::info!("artifacts: {origin}");
                    Some(origin)
                }
                Err(e) => {
                    log::warn!("artifacts: could not bind the loopback listener: {e}");
                    None
                }
            })
            .as_deref()
    }

    /// Register the session directories under the remembered roots in a background
    /// thread, so a slow volume does not hold up boot. A collision — one id under
    /// two roots — is reported and nothing else happens
    /// (docs/session-title.md). The receiver turns `true` when the thread is done,
    /// which is what the boot resume waits for.
    fn backfill_session_ids(
        session_ids: &crate::session_ids::SessionIds,
        remembered: &crate::remembered::Projects,
    ) -> tokio::sync::watch::Receiver<bool> {
        let (session_ids, remembered) = (session_ids.clone(), remembered.clone());
        let (done, passed) = tokio::sync::watch::channel(false);
        std::thread::spawn(move || {
            for collision in session_ids.backfill(&remembered.roots()) {
                log::warn!(
                    "session id {:?} is under both {} and {}; the id stays with the first",
                    collision.id,
                    collision.holder.display(),
                    collision.other.display()
                );
            }
            let _ = done.send(true);
        });
        passed
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
        self.adopt_prepared(id, runtime, created_by, detached, None)
    }

    /// [`adopt_with`](Self::adopt_with) for a runtime prepared from the starting entry
    /// `from`, which the session replaces under its id.
    pub(crate) fn adopt_prepared(
        self: &Arc<Self>,
        id: String,
        runtime: SessionRuntime,
        created_by: Option<String>,
        detached: bool,
        from: Option<&Arc<starting::Starting>>,
    ) -> Result<(), String> {
        let session = Arc::clone(runtime.session());
        // The web client addresses an adopted session under the host's own
        // mount, and a push notification opens that path (docs/web-push.md).
        session.set_web_path(format!("/sessions/{id}/"));
        session.set_attention(Arc::clone(&self.attention));
        session.set_newer_version(Arc::clone(&self.newer_version));
        if let Some(origin) = self.artifact_origin() {
            session.set_artifact_origin(origin.to_owned());
        }
        let hosted = Hosted {
            files: Files::rooted_at(session.cwd()),
            session: Arc::clone(&session),
            swansong: runtime.swansong().clone(),
            created_by: created_by.clone(),
            supervisor: created_by.clone(),
            detached,
            succeeds: None,
            subscribers: BTreeMap::new(),
            read_cursors: BTreeMap::new(),
            options: SessionOptions::default(),
        };
        // First one in wins: an id held by anything but the starting entry this
        // runtime was prepared from refuses, rather than being replaced and leaving a
        // running session unreachable. Creation and resume reserve the id as a
        // starting entry before they prepare, so this is the backstop.
        if !self
            .sessions
            .lock()
            .unwrap()
            .insert_hosted(&id, hosted, from)
        {
            // This runtime is live and will never be in the map, so it is shut
            // down here rather than left running unhosted.
            runtime.swansong().shut_down();
            return Err(format!("session {id:?} is already hosted"));
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
        tokio::spawn(runtime.swansong().interrupt(subscriptions::watch_status(
            Arc::downgrade(self),
            id.clone(),
            Arc::clone(&session),
        )));
        let host = Arc::clone(self);
        tokio::spawn(async move {
            let status = match runtime.finished().await {
                Ok(()) => {
                    log::info!("session {id}: ended");
                    status::SessionStatus::Ended
                }
                Err(e) => {
                    log::warn!("session {id}: ended with error: {e}");
                    status::SessionStatus::Failed {
                        error: e.to_string(),
                    }
                }
            };
            // The end is the last status the subscribers hear; then the
            // session's own subscriptions and its non-detached children go
            // with it (docs/tools.md).
            // An archived session has nothing left to announce.
            session.cancel_delayed_push(crate::log::PushCancel::Archived);
            host.post_status(&id, &session, &status);
            host.drop_subscriptions_of(&id);
            for child in host.cascade_of(&id) {
                if let Some(done) = host.end(&child) {
                    done.await;
                }
            }
            host.sessions.lock().unwrap().remove_hosted(&id);
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
            // A gate escalated to this session goes to the operator now, by the same
            // fail-safe; its holder's watcher re-routes it once woken.
            for holder in host.escalated_to(&id) {
                holder.touch_state();
            }
        });
        Ok(())
    }

    /// The session map, locked — for the subscription table (`subscriptions.rs`).
    pub(super) fn sessions_mut(&self) -> std::sync::MutexGuard<'_, Sessions> {
        self.sessions.lock().unwrap()
    }

    /// The session `id` succeeds, when one handed off to it (docs/tools.md).
    pub fn predecessor(&self, id: &str) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .hosted(id)
            .and_then(|hosted| hosted.succeeds.clone())
    }

    /// The session that commissioned `id`, when a session did (docs/tools.md).
    /// Control state — the creation tree, never logged or projected. A starting
    /// session's creator is the one its creation named.
    pub fn created_by(&self, id: &str) -> Option<String> {
        match self.sessions.lock().unwrap().entry(id)? {
            Entry::Hosted(hosted) => hosted.created_by.clone(),
            Entry::Starting(starting) => starting.created_by().map(str::to_string),
        }
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
    ) -> Result<String, String> {
        let Some(hosted) = self.hosted(from) else {
            return Err(format!("session {from:?} is not hosted"));
        };
        body.detached = hosted.detached;
        body.options = hosted.options.clone();
        let lineage = crate::env::Lineage {
            created_by: hosted.created_by.clone(),
            succeeds: Some(from.to_string()),
        };
        // The succession is recorded once the successor has started, so a predecessor
        // never names a session that failed to.
        let starting = self.create(body, lineage)?;
        starting.finished().await?;
        let id = starting.id().to_string();
        self.record_succession(from, &id, brief);
        Ok(id)
    }

    /// Record the inference options `id` was created with, which a `hand_off`
    /// successor is created with in turn.
    pub(super) fn record_options(&self, id: &str, options: SessionOptions) {
        if let Some(hosted) = self.sessions.lock().unwrap().hosted_mut(id) {
            hosted.options = options;
        }
    }

    /// The succession half of [`hand_off`](Self::hand_off), once the successor
    /// exists: it may archive `from`, its first turn is `brief` attributed to
    /// `from`, and `from` names it for the clients to follow.
    pub(crate) fn record_succession(&self, from: &str, successor: &str, brief: String) {
        if let Some(hosted) = self.sessions.lock().unwrap().hosted_mut(successor) {
            hosted.succeeds = Some(from.to_string());
        }
        if let Some(session) = self.session(successor) {
            session.submit_turn(
                SubmittedTurn::new(brief, mint_submission_id())
                    .sourced(Some(crate::frontend::handoff_source(from))),
            );
        }
        if let Some(predecessor) = self.session(from) {
            predecessor.set_successor(successor);
        }
        self.touch_listing();
    }

    /// The ids of the sessions on this host, starting or hosted, sorted
    /// (docs/control-api.md).
    pub fn ids(&self) -> Vec<String> {
        self.sessions.lock().unwrap().ids()
    }

    /// The hosted sessions holding a gate last escalated to `id` (docs/permissions.md).
    pub(crate) fn escalated_to(&self, id: &str) -> Vec<Arc<Session>> {
        let sessions: Vec<Arc<Session>> = self
            .sessions
            .lock()
            .unwrap()
            .hosted_iter()
            .map(|(_, hosted)| Arc::clone(&hosted.session))
            .collect();
        sessions
            .into_iter()
            .filter(|session| session.escalated_to().is_some_and(|(to, _)| to == id))
            .collect()
    }

    /// The running session under `id`; `None` for one still starting.
    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .hosted(id)
            .map(|h| Arc::clone(&h.session))
    }

    fn hosted(&self, id: &str) -> Option<Hosted> {
        self.sessions.lock().unwrap().hosted(id).cloned()
    }

    /// The entry under `id`, starting or hosted.
    fn entry(&self, id: &str) -> Option<Entry> {
        self.sessions.lock().unwrap().entry(id).cloned()
    }

    /// Put a starting entry under `id`, working in `cwd`, with no preparation behind
    /// it: a test drives its end itself, by adopting a runtime as a preparation does.
    #[cfg(test)]
    pub(crate) fn reserve_starting(
        &self,
        id: &str,
        cwd: &std::path::Path,
    ) -> Arc<starting::Starting> {
        let starting = starting::Starting::new(starting::StartingParts {
            id: id.to_string(),
            directories: crate::policy::Directories::at(cwd),
            resume: false,
            created_by: None,
            detached: false,
        });
        assert!(self.sessions.lock().unwrap().insert_starting(&starting));
        self.touch_listing();
        starting
    }

    /// The session under `id` while it is starting.
    pub(crate) fn starting(&self, id: &str) -> Option<Arc<starting::Starting>> {
        match self.entry(id)? {
            Entry::Starting(starting) => Some(starting),
            Entry::Hosted(_) => None,
        }
    }
}

/// Run a host: bind the control socket and the optional TCP listener over
/// `host`, and serve until shutdown. Sessions arrive by `POST /sessions`.
pub async fn serve_host(
    host: Arc<Host>,
    socket: PathBuf,
    bind: Option<TcpBind>,
) -> Result<(), Error> {
    // The state beside this socket and the socket itself are the host's whole
    // on-disk footprint; tighten them before anything is served from here
    // (docs/state-store.md). The directory is created when missing so the
    // first boot is tightened too, not only later ones.
    if let Some(dir) = socket.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("could not prepare {}: {e}", dir.display());
        }
        permissions::tighten_state_dir(dir);
    }
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
    // The bind minted the socket under the process umask; owner-only is the
    // contract (docs/control-channel.md) — a connect needs write permission,
    // so this is exactly the accounts that may reach the channel.
    permissions::tighten_socket(&socket);
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
        match auth.mint_link() {
            Ok(link) => {
                log::info!("  {link}");
                if open && webbrowser::open(&link).is_err() {
                    log::warn!("could not open a browser for {link}");
                }
            }
            Err(e) => log::warn!("could not mint a sign-in link: {e}"),
        }
        _tcp_server = Some(handle);
    }
    // The listener's shutdown completes only when every session's teardown
    // guard has dropped: the runtimes are children of the same `Swansong`.
    server.await;
    // Every session has released its binding by now; this reaps what is still starting
    // or running (docs/tools.md).
    crate::tools::lsp::shut_down(Arc::clone(host.language_servers())).await;
    Ok(())
}
