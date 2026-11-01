//! The host: one process owning many sessions and serving as their control
//! endpoint (docs/control-channel.md).
//!
//! A [`Host`] holds a map of sessions under one root `Swansong`: each one starting
//! while its environment and MCP mounts are prepared (`entries.rs`,
//! `preparation.rs`), then a started [`SessionRuntime`]. Nothing is forwarded: the
//! session's handlers run in this process against its `Session` — the host's own
//! routes plus `/sessions/<id>/…`, dispatched into the unchanged control routes
//! (docs/control-api.md).

use crate::host::runtime::SessionRuntime;
use crate::inference::SessionOptions;
use crate::protocol::sessions::{
    ArchiveOutcome, ArchivedSession, CreateSession, DormantSession, ListedSession, ListedStatus,
    PatchSession, SessionArchived, SessionState,
};
use crate::protocol::turns::mint_submission_id;
use crate::record::log::Notification;
use crate::record::log::Payload;
use crate::record::log::Seq;
use crate::record::persist::Log;
use crate::session::Session;
use crate::session::attention;
use crate::session::files_view::Files;
use crate::session::newer_version;
use crate::session::status;
use crate::session::submitted::SubmittedTurn;
use futures_lite::Stream;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Notify;
use trillium_sse::Eventable;
use trillium_tokio::Swansong;

pub mod boot;
pub mod discard;
pub mod inventory;
pub mod island;
pub(crate) mod permissions;
pub mod remembered;
pub mod runtime;
pub mod session_ids;
pub mod slug;
pub(crate) mod starting;
pub(crate) mod subscriptions;
pub mod worktree;

mod create;

pub(crate) mod resume;

mod archived;

pub(crate) mod listing;

mod supervise;
pub use supervise::SupervisorRefused;

pub(crate) mod entries;
use entries::{Entry, Sessions};

mod preparation;

mod boot_resume;

mod end;
use end::Departure;
pub use end::Ended;

mod island_exit;

mod agents;

mod attention_report;

mod session_control;

mod update_check;

#[cfg(test)]
mod testing;

#[cfg(test)]
mod test;

/// One process's sessions and the `Swansong` they all hang under.
pub struct Host {
    swansong: Swansong,
    pub(crate) inference: crate::inference::InferenceRegistry,
    /// Each connection's model listing, stored in the user state database
    /// (docs/model-catalog.md).
    pub(crate) catalog: crate::inference::catalog::Catalog,
    /// Where sessions have been launched, in the same database (docs/control-api.md).
    /// Written by every successful creation and read by the index route.
    pub(crate) remembered: remembered::Projects,
    /// The host-wide register of session ids, in the same database
    /// (`session_ids.rs`, docs/session-title.md).
    session_ids: session_ids::SessionIds,
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
    inventory: inventory::Inventory,
    /// Turns `true` when the host-start pass (the id backfill) has run; the boot
    /// resume waits for it (`boot_resume.rs`).
    start_pass: tokio::sync::watch::Receiver<bool>,
    /// Set when an owner watch adopts this host, which makes it an island
    /// (`island_exit.rs`, docs/operating.md).
    island: AtomicBool,
    /// The host's own environment, captured once at start (docs/env.md): the base a
    /// session launched without a caller-supplied environment inherits.
    /// `EnvSnapshot` omits Debug and Serialize, so it cannot be logged or echoed.
    host_env: crate::config::env_table::EnvSnapshot,
    /// Signalled when the session listing may have moved (`listing.rs`): a
    /// session arriving or leaving, and every wake of a session's status
    /// watcher. Every listing subscriber recomputes and diffs for itself, so
    /// this carries no value and an extra ping costs a comparison.
    listing_changed: Notify,
    /// What every client has last said about whether anyone is attending
    /// (`attention.rs`, docs/web-push.md). Shared with each adopted session, so
    /// a push for any of them reads one table.
    pub(crate) attention: Arc<attention::Attention>,
    /// What the update check last found newer than this host (`update_check.rs`,
    /// docs/update-check.md). Shared with each adopted session, which publishes it as
    /// its `newer_version` topic; empty on a host that does not check.
    newer_version: Arc<newer_version::NewerVersionCell>,
    /// What starts the artifact-only loopback listener on this host and answers its
    /// origin (docs/artifacts.md). The listener's module (`artifacts.rs`) sits above the
    /// host, so the process that serves this host installs it
    /// ([`Self::set_artifact_listener`]) before anything is adopted.
    artifact_listener: std::sync::OnceLock<StartArtifactListener>,
    /// The artifact-only loopback listener's origin (docs/artifacts.md): started by
    /// the first adopt, under this host's `Swansong`; `None` once its bind failed, so
    /// the bind is tried once.
    artifact_origin: std::sync::OnceLock<Option<String>>,
    /// What builds the in-session `agents__*` server a hosted session mounts
    /// (docs/tools.md). The verbs' module (`control_tools`) sits above the host, so the
    /// process that serves this host installs it ([`Self::set_session_control`]) before
    /// anything is adopted (`session_control.rs`).
    session_control: std::sync::OnceLock<MountSessionControl>,
    /// One lock per MCP server for refreshing its stored grant, shared by the token
    /// store every session here mounts with, so the host's sessions refresh a
    /// server's grant once however many find it expired (docs/mcp-auth.md).
    mcp_refresh_locks: crate::tools::mcp::auth::store::RefreshLocks,
    /// The language servers behind the `lsp` built-ins (docs/tools.md). Workspaces are
    /// keyed by project root, so the sessions in one project share one set of servers;
    /// each session's binding releases its interest when its toolbox drops, and
    /// [`serve_host`](crate::listeners::serve::serve_host) stops what remains after the sessions have ended.
    language_servers: Arc<crate::tools::lsp::LanguageServers>,
}

/// Installed by the process serving the host ([`Host::set_artifact_listener`])
/// (docs/artifacts.md).
pub type StartArtifactListener = fn(&Arc<Host>) -> io::Result<String>;

/// Installed by the process serving the host ([`Host::set_session_control`])
/// (docs/tools.md).
pub type MountSessionControl = fn(&Arc<Host>, String) -> Box<dyn crate::tools::ControlServer>;

/// What the host keeps per session: the state handlers read, the child
/// `Swansong` that ends it, and its `/files` view rooted at its own cwd.
#[derive(Clone)]
pub(crate) struct Hosted {
    pub(crate) session: Arc<Session>,
    swansong: Swansong,
    pub(crate) files: Files,
    /// How the entry leaves the map (`end.rs`): an end under way holds the removal back
    /// until it has recorded the session, and resolves once the entry has gone.
    departure: Departure,
    /// The session that commissioned this one, when a session did (docs/tools.md).
    /// Control state only: never logged, never projected.
    pub(crate) created_by: Option<String>,
    /// The session that supervises this one (docs/permissions.md). Starts as
    /// `created_by`, and moves only by `Host::set_supervisor` (`supervise.rs`).
    /// Control state only: not persisted, so a resume starts again from the creator.
    pub(crate) supervisor: Option<String>,
    /// Whether this session outlives its creator (docs/tools.md).
    pub(crate) detached: bool,
    /// The session that handed its work to this one (docs/tools.md) — the one
    /// session outside its descendants this one may archive.
    pub(crate) succeeds: Option<String>,
    /// Who hears this session's status, and on what terms (`subscriptions.rs`).
    pub(crate) subscribers: BTreeMap<String, subscriptions::Subscription>,
    /// The last seq each reading session was shown of this one's transcript
    /// (`agents.rs`, docs/tools.md). Control state only: never logged, gone with a
    /// restart, when a reader's next default read starts over at the tail.
    pub(crate) read_cursors: BTreeMap<String, Seq>,
    /// The inference options this session was created with — model, spotter,
    /// triage, effort, temperature — which a `hand_off` successor is created
    /// with in turn (docs/tools.md). Empty for a session adopted without a
    /// creation request.
    pub(crate) options: SessionOptions,
}

impl Host {
    /// `host_env` is the host's own environment at start, captured by the caller
    /// that loaded the inference registry from it (docs/env.md).
    pub fn new(
        inference: crate::inference::InferenceRegistry,
        host_env: crate::config::env_table::EnvSnapshot,
    ) -> Arc<Self> {
        let catalog = crate::inference::catalog::Catalog::new(
            inference.history().state_path().map(ToOwned::to_owned),
        );
        let inference = inference.with_catalog(catalog.clone());
        let session_ids = inference
            .history()
            .state_path()
            .map_or_else(session_ids::SessionIds::disabled, |path| {
                session_ids::SessionIds::at(path.to_owned())
            });
        let remembered = inference
            .history()
            .state_path()
            .map_or_else(remembered::Projects::disabled, |path| {
                remembered::Projects::at(path.to_owned())
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
            inventory: inventory::Inventory::new(),
            start_pass,
            island: AtomicBool::new(false),
            host_env,
            listing_changed: Notify::new(),
            attention: Arc::default(),
            newer_version: Arc::default(),
            artifact_listener: std::sync::OnceLock::new(),
            artifact_origin: std::sync::OnceLock::new(),
            session_control: std::sync::OnceLock::new(),
            mcp_refresh_locks: Default::default(),
            language_servers: crate::tools::lsp::language_servers(),
        })
    }

    pub(crate) fn language_servers(&self) -> &Arc<crate::tools::lsp::LanguageServers> {
        &self.language_servers
    }

    pub(crate) fn mcp_refresh_locks(&self) -> crate::tools::mcp::auth::store::RefreshLocks {
        self.mcp_refresh_locks.clone()
    }

    /// Install what starts the artifact-only loopback listener (docs/artifacts.md),
    /// before the first adopt. The first install stands.
    pub fn set_artifact_listener(&self, start: StartArtifactListener) {
        let _ = self.artifact_listener.set(start);
    }

    /// The artifact-only loopback listener's origin, starting the listener on first
    /// use (docs/artifacts.md). It serves only artifact routes, so it is started for
    /// any host, with or without a TCP bind. `None`, with nothing started, on a host
    /// no listener was installed on.
    pub(crate) fn artifact_origin(self: &Arc<Self>) -> Option<&str> {
        let start = self.artifact_listener.get()?;
        self.artifact_origin
            .get_or_init(|| match start(self) {
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
        session_ids: &session_ids::SessionIds,
        remembered: &remembered::Projects,
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
    /// `Swansong` — leaves the map when its teardown has run, and one that
    /// [`end`](Self::end) stops leaves once that end has recorded it. The runtime
    /// must have been started under [`swansong`](Self::swansong).
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
        let departure = Departure::default();
        let hosted = Hosted {
            files: Files::rooted_at(session.cwd()),
            session: Arc::clone(&session),
            swansong: runtime.swansong().clone(),
            departure: departure.clone(),
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
            session.cancel_delayed_push(crate::record::log::PushCancel::Archived);
            host.post_status(&id, &session, &status);
            host.drop_subscriptions_of(&id);
            for child in host.cascade_of(&id) {
                if let Some(done) = host.end(&child) {
                    done.await;
                }
            }
            // An end under way on this session has its record written first, and
            // resolves once the entry has gone (`end.rs`).
            host.remove_when_recorded(&id, &departure).await;
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
    pub(crate) fn sessions_mut(&self) -> std::sync::MutexGuard<'_, Sessions> {
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
    /// Hand `from`'s work to a new session (docs/tools.md), whose first turn is
    /// `brief`, attributed to `from`. `from` records the successor, which attached
    /// clients follow. Returns the successor's id.
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
        let lineage = crate::config::env::Lineage {
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
    pub(crate) fn record_options(&self, id: &str, options: SessionOptions) {
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
                    .sourced(Some(crate::record::names::sources::handoff_source(from))),
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
    pub(crate) fn entry(&self, id: &str) -> Option<Entry> {
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
            directories: crate::config::policy::Directories::at(cwd),
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
