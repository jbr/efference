//! A session's state (docs/control-channel.md): [`Session`], which the agent loop and the
//! control channel's handlers share, and the modules beneath it that each own a part of
//! it.
//!
//! The host holds the sessions and the handlers serve them; this group imports neither
//! (docs/README.md, *Dependency direction*).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{Notify, broadcast, mpsc, oneshot};

use crate::inference::api::{
    Client, RequestUsage, Streamed, ToolChoice, UpstreamFailureState, UpstreamRetryState, Usage,
    upstream_failure_state, upstream_retry_state,
};

use crate::config::{ConfigSource, LoadedConfig, Refreshed};
use crate::protocol::state::{
    BlockedReason, ConfigRepair, LoopPhase, PendingProposal, SessionIdentity, Stall, StallReason,
    Topic,
};
use crate::protocol::stream::StreamDelta;
use crate::record::log::{
    Approval, CallId, EditState, LogEntry, Notification, Payload, RawResult, RequestOrigin, Seq,
    ToolCall, Turn,
};
use crate::record::persist::Log;
use crate::record::project::{
    ApiMessage, ImagePlacement, Retention, WireTurn, project_semantic, project_with,
};
use crate::tools::Interrupt;
use crate::tools::process_monitor::{BgView, ProcessMonitor};
use submitted::SubmittedTurn;

/// The prefix session-scoped log lines carry, the shape the lifecycle
/// announcements use (docs/control-channel.md). Empty when there is no id —
/// tests, offline sessions — leaving the line unattributed.
pub(crate) fn session_id_prefix(id: Option<&str>) -> String {
    id.map_or_default(|id| format!("session {id}: "))
}

/// Code a test interleaves at one point in a session's work, run once there
/// (`between_reads`, `between_pushes`, `before_archived`, `before_request`).
#[cfg(test)]
type Interleaving = Box<dyn FnOnce(&Session) + Send>;

/// A session's state and control surface (docs/control-channel.md).
/// The loop and HTTP handlers hold the same `Arc<Session>`; the runtime owns the
/// listener and wires the agent future and auxiliary tasks.
pub struct Session {
    /// Immutable: the project (config, grants) and the working directory (tools and
    /// child execution), which differ only inside a worktree (docs/control-channel.md).
    directories: crate::config::policy::Directories,
    /// The session's id — the storage directory's name, known at boot
    /// (docs/control-channel.md). None in tests and offline sessions.
    id: Option<String>,
    /// The parent of the session's storage dir — the `.efference/` base every
    /// session in this project shares (docs/tools.md). The in-session `sessions`
    /// mount scopes its listing and target resolution to it. Set once by the
    /// runtime; unset in tests and offline sessions.
    storage_base: std::sync::OnceLock<PathBuf>,
    /// The attachment store (docs/inbox.md), set once by the runtime alongside
    /// its watcher. Unset in tests and offline sessions, where the upload
    /// routes answer that there is nowhere to put a file.
    attachment_store: std::sync::OnceLock<attachments::Store>,
    /// The path the web client addresses this session by (docs/web-bind.md):
    /// `/` on the session's own bind, `/sessions/<id>/` under a host. Set once
    /// by whoever adopts the session — the bind's shape belongs to whoever
    /// binds, never to a session (docs/web-client.md) — and read by the push
    /// sender: a notification opens this path (docs/web-push.md).
    web_path: std::sync::OnceLock<String>,
    /// The key the session's artifact links are signed under (docs/artifacts.md):
    /// random per session, so the links stop verifying when the session is gone and a
    /// resumed session issues new ones.
    artifact_key: [u8; artifact_links::KEY_BYTES],
    /// The provider's cache lifetime, set at boot only when the model has a prompt
    /// cache; the `cold` topic reads it (`session/cold.rs`).
    cache_ttl: std::sync::OnceLock<Duration>,
    /// When a resume on another main model abandoned the prefix the usage record's
    /// requests had cached; set once at boot (`session/cold.rs`).
    cache_lost: std::sync::OnceLock<crate::record::log::Timestamp>,
    /// A person acted while the session was believed cold (a turn, continue, decision or
    /// answer), consumed by the loop's next cold check (`session/cold.rs`).
    person_acted: AtomicBool,
    /// The cold check has recorded a park and not yet its end: its own bookkeeping.
    /// What clients read is the `cold_parked` loop phase, which a person's action ends
    /// before the loop wakes (`phase.rs`).
    cold_parked: AtomicBool,
    /// Wakes the cold check and the held inbox wake when a person acts.
    cold_signal: Notify,
    /// Wakes the compaction task when a compaction is asked for; the compaction's
    /// state is folded from the log with the operations (`session/operations.rs`).
    compaction_signal: Notify,
    /// The loop appends under this lock; handlers snapshot it for `GET /log`.
    /// The lock is only ever held across the
    /// synchronous `project`/append — never an `await`.
    log: Mutex<Log>,
    /// The open requests and running tools, folded from the log as it grows
    /// (`session/operations.rs`). Locked after `log`, never before.
    operations: Mutex<operations::Operations>,
    /// Where tool-result images land on the wire (docs/log-and-projection.md):
    /// provider-derived at boot, fixed for the session.
    image_placement: ImagePlacement,
    /// Sender half of the turn inbox; handlers push, the loop drains the receiver.
    turns_tx: mpsc::UnboundedSender<SubmittedTurn>,
    /// In-flight approval gates keyed by the `tool_use` `CallId` (already unique,
    /// already on the wire — no new id scheme).
    approvals: Mutex<HashMap<CallId, oneshot::Sender<Approval>>>,
    /// The single pending approval, surfaced by `GET /state` (the loop is
    /// sequential — one gate at a time, docs/README.md).
    pending: Mutex<Option<Pending>>,
    /// The link to the host that holds this session's supervisor pointer — who
    /// answers its gates when the dial reads `supervisor` (docs/permissions.md) — and
    /// can say whether that session is still running. Set by `Host::adopt_with`;
    /// `None` for a session built without a host.
    supervisor: Mutex<Option<review::SupervisorLink>>,
    /// Fired by `POST /loop/interrupt`; the future `interrupted()` races a running tool,
    /// and the agent client's read loop races it against the stream (docs/control-channel.md).
    interrupt: Arc<Notify>,
    /// The **latched** half of `POST /loop/interrupt` (docs/control-channel.md). `Notify::notify_waiters`
    /// wakes only a *currently registered* waiter and stores no permit, so an interrupt fired while
    /// no backend awaits the abort future is dropped; the loop reads this flag at the junctures
    /// either side of a call — and the next model request reads it too
    /// (`Client::with_interrupt_latch`) — and stops there instead.
    interrupt_latch: Arc<AtomicBool>,
    /// Fired by `PATCH /processes/:id {background: true}` — the detach half of the interrupt intent
    /// (docs/process-monitor.md, the foreground→background handoff). Like `interrupt`,
    /// `notify_waiters` reaches only a *currently-awaited* race, so a detach
    /// request with no foreground tool running is lost harmlessly.
    detach: Notify,
    /// The operator note optionally attached to a `PATCH /processes/:id {background: true}`
    /// (docs/process-monitor.md): stashed by the handler, consumed by the loop only
    /// when a detach actually lands, and cleared when a fresh foreground bash
    /// starts so a request that never found a running tool can't mislabel a
    /// later handoff.
    detach_note: Mutex<Option<String>>,
    /// Pinged on each log append; the `GET /events` tail wakes on it.
    appended: Notify,
    /// The host's attention table, handed over at adopt (`attention.rs`,
    /// docs/web-push.md). Unset for a session outside a host, which treats nobody
    /// as attentive.
    attention: std::sync::OnceLock<Arc<attention::Attention>>,
    /// The host's update-check answer, handed over at adopt (`newer_version.rs`,
    /// docs/update-check.md). Unset for a session outside a host, which publishes
    /// `newer_version: null`.
    newer_version: std::sync::OnceLock<Arc<newer_version::NewerVersionCell>>,
    /// The push waiting out [`delayed_push::PUSH_DELAY`] because a client was
    /// attending when its moment fired (`delayed_push.rs`, docs/web-push.md). At most
    /// one; resolution and sightings of the moment empty it.
    delayed_push: delayed_push::DelayedPushSlot,
    /// The push bookkeeping (`push.rs`): the session's own handle for the tasks
    /// that append push records, the moment counter, and the test stand-in for
    /// the push services.
    pushes: push::PushState,
    /// Where the loop is (`phase.rs`, docs/control-api.md): one exclusive value the loop
    /// sets from its control flow. Read with `queue` as one [`PhaseAndQueue`], it drives
    /// the `/settle` long-poll's "at a decision point?" predicate ([`Session::is_settled`]).
    ///
    /// [`PhaseAndQueue`]: settle::PhaseAndQueue
    phase: Mutex<LoopPhase>,
    /// How many trajectories have ended into idle: what tells a status watcher that the
    /// idle it reads followed a turn, rather than an idle triage that pinned nothing
    /// (`subscriptions.rs`).
    turns_ended: std::sync::atomic::AtomicU64,
    /// Whether the loop has gone idle at least once. Only tests read it: one that stands
    /// in for the loop — writing its phase, holding a gate — waits for this first, since
    /// the loop's first idle writes the phase and would overwrite what the test wrote.
    #[cfg(test)]
    pub(crate) loop_idled: AtomicBool,
    /// Run once by the next [`Session::phase_and_queue`], between its read of the phase and
    /// its read of the queue: a test's stand-in for a writer landing there.
    #[cfg(test)]
    pub(crate) between_reads: Mutex<Option<Interleaving>>,
    /// Run once by the next [`Session::queue_turn`], between its push onto the queue and the
    /// hold's decision: a test's stand-in for a reader landing there.
    #[cfg(test)]
    pub(crate) between_pushes: Mutex<Option<Interleaving>>,
    /// Run once by the next archive of this session (`Host::end`), just before its
    /// `Archived` entry is appended: a test's stand-in for the host's watcher landing there.
    #[cfg(test)]
    pub(crate) before_archived: Mutex<Option<Interleaving>>,
    /// Run once by the loop's next model request, after the loop's last juncture and before
    /// the request arms its abort: a test's stand-in for a press landing there.
    #[cfg(test)]
    pub(crate) before_request: Mutex<Option<Interleaving>>,
    /// Turns submitted (`POST /turns`) but not yet dequeued by the loop, in submission order —
    /// the **authority** on what is pending (docs/control-channel.md). The loop **claims** each
    /// received turn against this queue and drops one that is no longer here — that is how a
    /// withdrawn turn is never appended. A pending turn is ephemeral: it is not in the log, so
    /// withdrawing one leaves no trace to retract.
    queue: Mutex<std::collections::VecDeque<SubmittedTurn>>,
    /// The `ask` awaiting a reply, if any (docs/process-monitor.md). Taken by the next
    /// submitted turn, which it resolves instead of queueing. Lock order: this before
    /// `pace` and `queue`.
    ask: Mutex<Option<ask::AskState>>,
    /// Pinged whenever the daemon may have reached a **settled** state — a gate appeared
    /// ([`register_pending`]) or the loop went idle ([`going_idle`]). The `/settle` long-poll waits on it.
    ///
    /// [`register_pending`]: Session::register_pending
    /// [`going_idle`]: Session::going_idle
    settle_changed: Notify,
    /// A `POST /loop/continue` awaiting the loop (docs/control-api.md): the operator
    /// handed control back with no input, so the loop takes another turn on the wire
    /// it already has. A flag rather than a queued message because a continue carries
    /// nothing — two presses are one continue — and because it must survive the gap
    /// between the request and the idle `select!` registering its waiter. Cleared by
    /// [`Session::take_continue`] when the loop consumes it; never logged, never
    /// projected. Written only under the `queue`
    /// lock, so [`Session::phase_and_queue`] reads it at the instant it reads the phase.
    continue_pending: AtomicBool,
    /// Wakes the idle loop when [`continue_pending`](Self::continue_pending) is set.
    /// Paired with the flag rather than standing alone: `notify_waiters` stores no
    /// permit, so the flag is what a wake landing before the loop parks lands on.
    continue_signal: Notify,
    /// How many sessions this one is subscribed to (docs/tools.md) — host control
    /// state mirrored here so the `wait` tool can count a subscribed session as
    /// something that may wake it (docs/process-monitor.md).
    subscriptions: std::sync::atomic::AtomicUsize,
    /// Pinged when a notification is enqueued (docs/inbox.md — `notify`). The agent loop's
    /// idle path awaits this to wake an idle agent for a standalone triage.
    notified: Notify,
    /// How many entries the log held when this session was built: a resumed session's
    /// restored history, `0` for a new one. A notification at or past this seq arrived
    /// after the resume; the idle wake looks only at those (docs/resumption.md).
    restored_len: usize,
    /// Fan-out for ephemeral pre-commit output deltas (docs/README.md). Producers (the
    /// loop's `complete` sink and the bash tool) publish via [`Session::publish_delta`];
    /// each control-stream connection wanting `output` frames subscribes. Lossy by
    /// design — see [`StreamDelta`].
    stream_tx: broadcast::Sender<StreamDelta>,
    /// Pinged when a state topic may have moved (docs/control-api.md). One notify for
    /// every topic, not one each — see `topics.rs`.
    state_changed: Notify,
    /// Fan-out for level-triggered state updates — the whole-topic replacements that a
    /// `GET /events` connection turns into `state` frames. Fed by the one diffing task,
    /// so N clients cost one recomputation (`topics.rs`).
    state_tx: broadcast::Sender<StateUpdate>,
    /// Cache/usage telemetry — the most recent request's `usage` and a wall-clock
    /// stamp, surfaced by `GET /state` so clients can see cache hit/miss and
    /// predict TTL eviction (docs/log-and-projection.md). Transport-only: never touches the log.
    metrics: Mutex<Metrics>,
    /// The per-request usage record (docs/log-and-projection.md): one JSONL line per
    /// upstream request, appended to the session dir's `usage.jsonl`. Usage must stay out
    /// of the projection, so this is never the canonical log. `None` (tests, offline)
    /// drops the lines; the in-memory `metrics` fold is unaffected.
    usage_log: Mutex<Option<io::BufWriter<std::fs::File>>>,
    /// The last non-success upstream response, shared with the HTTP client and
    /// surfaced by `GET /state`. Transport-only: never touches the log or projection
    /// (docs/transport.md).
    upstream_failure: UpstreamFailureState,
    /// The retry in flight, shared with the HTTP client and surfaced by `GET /state`
    /// as its own level-triggered topic. Unlike the failure cell above it carries its
    /// own change signal — a retry pause is *not* followed by an append, so
    /// `watch_topics`'s wake-on-`appended` fallback would leave the topic late for
    /// the whole pause (docs/transport.md). Transport-only: never logged or
    /// projected.
    upstream_retry: UpstreamRetryState,
    /// The Codex backend's subscription windows as of the last response that
    /// carried them (`api/plan.rs`), shared with the HTTP client and published
    /// under `usage.plan`. `None` for every other provider. Transport-only.
    plan: crate::inference::api::PlanState,
    /// The separate triage client's own failure and plan cells (docs/control-api.md),
    /// set once at boot when the triage runs on its own model and never set when it
    /// borrows the agent's client. Their presence is what makes `triage_upstream_failure`,
    /// `usage.triage_plan`, and `usage.triage_cold_streak` non-null. Transport-only.
    triage_client: std::sync::OnceLock<(UpstreamFailureState, crate::inference::api::PlanState)>,
    /// Cumulative triage retention (docs/triage.md), folded lazily from the
    /// log behind a cursor rather than accumulated at the append sites. Unlike the
    /// transport telemetry above this is **derived from the canonical record**, so a
    /// resumed session reports the whole session's ratio instead of restarting at
    /// zero. The fold is monotone (a result's `wire` is fixed at append), so the
    /// cursor never needs invalidating — see [`Session::retention`].
    retention: Mutex<(usize, Retention)>,
    /// The selected model's capabilities, fetched from the provider's metadata
    /// endpoint at boot (a required boot step — docs/transport.md) and set once
    /// via [`Session::set_capabilities`]. `None` only in tests and offline verbs
    /// that never fetched; `GET /state` then reports `context: null`.
    /// Transport-only: never logged or projected.
    capabilities: Mutex<Option<crate::inference::api::ModelCapabilities>>,
    /// Session cost telemetry (docs/transport.md): the running sum of
    /// per-generation **billed** costs fetched from the provider's accounting
    /// endpoint — authoritative, never modeled from a rate card. Behind its own
    /// `Arc` (like [`UpstreamFailureState`]) because the fetch runs as a spawned
    /// task after each recorded request. Armed only when the provider has a
    /// per-request cost surface (OpenRouter today); otherwise `GET /state`
    /// reports `cost: null`. Transport-only: never logged or projected.
    cost: Arc<Mutex<CostCell>>,
    /// What this session is — the configured model and its working directory
    /// (docs/control-api.md), set once by boot and read by the
    /// `session_identity` topic. `None` in tests and offline verbs that never
    /// booted a session, which the topic reports as `null`.
    identity: Mutex<Option<SessionIdentity>>,
    /// A trajectory that gave up (docs/transport.md), read by the `stall`
    /// topic. Written by the loop, which holds a [`Session`] and signals
    /// `state_changed` at both sites; `None` whenever a trajectory is running
    /// or last ran to completion. Control state: never logged, never projected
    /// — the durable record of the same event is the record-only entries and
    /// the notification the loop already appends.
    stall: Mutex<Option<Stall>>,
    /// The config-unparseable park (docs/permissions.md), read by the
    /// `config_repair` topic and awaited by the loop. Set by the config watcher
    /// on a failed revert, cleared by the same watcher the moment a refresh parses.
    config_repair: Mutex<Option<ConfigRepair>>,
    /// Fires when [`Self::config_repair`] clears, so the parked loop wakes without
    /// polling. Same lost-wakeup discipline as the pace hold: arm before reading.
    config_repaired: Notify,

    /// The in-memory queue of slot notifications (docs/inbox.md): out-of-band events a
    /// later one on the same slot supersedes, awaiting their first triage. A slot
    /// notification becomes a durable `Ambient` entry when a triage first shows it, and
    /// one superseded while queued is dropped here. A notification without a slot is a
    /// log entry from its arrival and is not held here.
    inbox: Mutex<Inbox>,
    /// The pending-proposals queue: async `ConfigEdit`s (the session's `propose_scope`, the
    /// spotter's prefix-promotion) awaiting an orchestrator's commit/cancel
    /// (docs/permissions.md). A `Proposed` entry is durable in the log, but its *pending*
    /// status lives here until resolved.
    proposals: Mutex<Vec<PendingProposal>>,
    /// The background-process registry (docs/process-monitor.md): live children whose running tail
    /// is re-sampled fresh each triage. Ephemeral control state like `inbox` /
    /// `metrics` — never logged, never projected (a running tail forced through the
    /// immutable notification model would pile up stale snapshots). Pinned tails and
    /// exits *do* reach the durable log, but as `Ambient` notifications, not from here.
    process_monitor: Mutex<ProcessMonitor>,
    /// Fired each time a background child is reaped (`drain_background` after
    /// `child.wait()`), so teardown can await the children actually being gone
    /// rather than the kill signal merely having been sent
    /// (docs/process-monitor.md).
    reaped: Notify,
    /// The resolved role config and where it came from (docs/permissions.md).
    /// Read-only for the session's lifetime — the scope is already frozen into the
    /// cached preamble; this copy backs the `GET /state` report ("which role am I?") and
    /// is where the cascade rungs (the prefix fast-path and the spotter) read the
    /// allowlist, path grants, scope, and watch-for from. Immutable, so no lock.
    role: LoadedConfig,
    /// The **effective** policy = the base `role` + the fold of every `ConfigEdit` appended so
    /// far (docs/permissions.md). Maintained incrementally in [`append`]
    /// as grants land, so a gate reads current policy in O(1) without re-scanning the log.
    /// Behind a lock because grants mutate it mid-session (unlike the frozen base `role`);
    /// the gate takes an `Arc` clone, cheap and safe to hold across the spotter's await.
    ///
    /// [`append`]: Session::append
    effective: Mutex<Arc<crate::config::Config>>,
    /// The live config file — the format-preserving document currently on disk plus what it
    /// validates to (docs/permissions.md). `None` when the role resolved
    /// to the built-in default (no file to hold). Behind a lock because both the loop task (a
    /// committed grant, and the check after an approved config write) and the fs-watch task reach
    /// it; every path in and out of the file goes through here, which is what keeps "the disk
    /// always holds the last content that parsed" true of *all* writers rather than some.
    config_file: Mutex<Option<crate::config::ConfigFile>>,
    /// The prefix-promotion probe ledger (docs/permissions.md): per-prefix
    /// seen-counts + probe votes, the state the descent reads to decide which prefix (if any) to ask
    /// the spotter to vouch for. Derived from the log's `Spot` entries (folded at startup via
    /// [`crate::config::policy::probe_ledger`], maintained incrementally as the loop assesses moves), never on
    /// the wire — like `effective`, behind a lock because the loop mutates it mid-session.
    probes: Mutex<crate::config::policy::ProbeLedger>,
    /// The session's resolved `[env]` policy (docs/env.md): the strip set every
    /// background spawn applies, and the masker installed on the log. Not behind
    /// a lock — the strip list is fixed at boot, and the masker owns its own
    /// append-only interior state, so a config edit adds needles without
    /// replacing the policy.
    env_policy: crate::config::env_table::EnvPolicy,
    /// The operator's **review level** (docs/permissions.md), stored as an
    /// `AtomicU8` ordinal so it is lock-free and **runtime-mutable** (the `PATCH /loop {review_level}` verb,
    /// not just a boot flag). The loop's cascade reads it ([`Session::review_level`]) to decide who
    /// reviews a gated call; every position but `Autonomous` holds the call — they differ in who
    /// is asked. Orthogonal to pace. (The **config-write** gate is never flipped by this — a
    /// session's edit to its own config is always held for the orchestrator-above.)
    review_level: AtomicU8,
    /// The operator's **pace** dial (docs/control-channel.md): a between-turns
    /// wall-clock control that is *transparent to the agent*. Pure operator-view state — never a
    /// log entry, never projected — so a held or slowed loop is byte-identical on the wire to a
    /// fast one (that, plus timestamps-never-projected, is the invisibility guarantee). Read at the
    /// between-step juncture by [`Session::pace_at_juncture`].
    pace: Mutex<Pace>,
    /// Wakes an in-flight [`pace_at_juncture`](Session::pace_at_juncture) sleep so a release or a submitted
    /// turn ends the wait *immediately* rather than after the remaining delay. Fired by
    /// [`submit_turn`](Session::submit_turn) (submit ⇒ resume, always) and by a release
    /// (`set_pace` dropping `held`); a pure speed change or a *hold* does not fire it (those take
    /// effect at the next juncture).
    pace_interrupt: Notify,
    /// The session scratch dir (docs/tools.md): minted by boot, exported to bash
    /// children as `$TMPDIR` (foreground via the toolbox's `Bash`, background via
    /// [`spawn_background`](Self::spawn_background)), granted rw in the injected
    /// path stack, and the one value the structured-tool expander resolves. All
    /// projections of this one value. `None` in tests and non-executing contexts.
    scratch: Option<PathBuf>,
    /// The frozen env map for structured-tool path expansion
    /// ([`crate::tools::expand`]): `{TMPDIR: scratch}` when a scratch dir exists, else
    /// empty — every `$NAME` then errors, which is the fail-closed default.
    expander: crate::tools::expand::Expander,
    /// The session's full tool catalog, keyed by wire name — every name the
    /// turn's `tools[]` can carry. Snapshotted by [`Session::set_tool_catalog`]
    /// before the toolbox moves into the loop, and read back by `GET /v1/tools/:name`
    /// (docs/control-api.md). The mounted set is fixed for the session
    /// (docs/log-and-projection.md), so this is written once and never reconciled.
    /// Empty in tests and offline verbs that never built a toolbox.
    tools: Mutex<HashMap<String, crate::tools::ToolEntry>>,
}

impl Session {
    /// The working directory: where tools and child processes run, and what a child
    /// session inherits as its own (docs/control-channel.md).
    pub fn cwd(&self) -> &std::path::Path {
        self.directories.working()
    }
    /// The directories the permission classifier resolves against (docs/permissions.md).
    pub fn directories(&self) -> crate::config::policy::Directories {
        self.directories.clone()
    }
    /// Name the session's web path (docs/web-bind.md): `/sessions/<id>/` under
    /// a host, `/` on the session's own bind. Called once, by the host's
    /// adopt; a session daemon never calls it and keeps the root default.
    pub fn set_web_path(&self, path: String) {
        let _ = self.web_path.set(path);
    }

    /// The session's id, the storage directory's name; `None` in tests and offline
    /// sessions.
    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The key this session's artifact links are signed under (docs/artifacts.md).
    pub(crate) fn artifact_key(&self) -> &[u8; artifact_links::KEY_BYTES] {
        &self.artifact_key
    }

    /// The path a push notification opens for this session (docs/web-push.md):
    /// the web path the client addresses it by, `/` before a host has named it.
    pub fn web_path(&self) -> &str {
        self.web_path.get().map_or("/", String::as_str)
    }

    /// Record the session's storage base — the parent of `SessionPaths::storage`,
    /// the `.efference/` every session in this project shares (docs/tools.md).
    /// Called once by the runtime; unset in tests and offline sessions.
    pub fn set_storage_base(&self, storage: &std::path::Path) {
        let base = storage
            .parent()
            .map_or_else(|| storage.to_path_buf(), std::path::Path::to_path_buf);
        let _ = self.storage_base.set(base);
    }

    /// The storage base this session shares with its project's other sessions,
    /// when the runtime set one (docs/tools.md).
    pub fn storage_base(&self) -> Option<&std::path::Path> {
        self.storage_base.get().map(PathBuf::as_path)
    }

    /// Log a session-scoped warning with the session's id prefixed, the shape
    /// the lifecycle announcements use (docs/control-channel.md). Unattributed
    /// when the session has no id (tests, offline sessions).
    pub fn warn(&self, args: std::fmt::Arguments<'_>) {
        log::warn!("{}{args}", session_id_prefix(self.id.as_deref()));
    }

    /// The same, at error level.
    pub fn error(&self, args: std::fmt::Arguments<'_>) {
        log::error!("{}{args}", session_id_prefix(self.id.as_deref()));
    }

    /// The environment every child this session spawns receives — the base its
    /// `bash` children get, strips and injections applied (docs/env.md). Handed
    /// to a session-driven `create` as the new session's base.
    pub fn child_env(&self) -> crate::config::env_table::ChildEnv {
        self.env_policy.child_env()
    }

    /// Snapshot the full session tool catalog for `GET /v1/tools/:name`
    /// (docs/control-api.md) — the same union `tools[]` advertises. Called once by
    /// the runtime before `toolbox` moves into the loop, which is why only a served
    /// session has one.
    pub fn set_tool_catalog(&self, catalog: HashMap<String, crate::tools::ToolEntry>) {
        *self.tools.lock().unwrap() = catalog;
    }

    /// One tool's full definition, addressed by its wire name — the answer to
    /// `GET /v1/tools/:name` (docs/control-api.md). `None` when no tool by
    /// that name was advertised this session, including every name in a
    /// session read with no daemon behind it (the catalog was never set).
    pub fn tool_entry(&self, wire_name: &str) -> Option<crate::tools::ToolEntry> {
        self.tools.lock().unwrap().get(wire_name).cloned()
    }

    /// Store the boot-fetched model capabilities (docs/transport.md). Called once
    /// by the runtime before the loop starts; the value is immutable
    /// thereafter and read by `GET /state`'s context readout.
    pub fn set_capabilities(&self, capabilities: crate::inference::api::ModelCapabilities) {
        *self.capabilities.lock().unwrap() = Some(capabilities);
    }

    /// Cumulative triage retention over the whole session (docs/triage.md),
    /// advancing the fold over entries appended since the last call.
    ///
    /// Deriving this from the log rather than accumulating it at the gate's append
    /// sites keeps one definition of "kept" (the projection's) and makes a resumed
    /// session report the whole session, not just what this process watched. Cheap
    /// to call per `GET /state` poll: steady state folds the handful of entries
    /// appended since the previous poll, and only a fresh process folds the backlog.
    pub fn retention(&self) -> Retention {
        let log = self.log.lock().unwrap();
        let entries = log.entries();
        let mut cell = self.retention.lock().unwrap();
        let (cursor, retention) = &mut *cell;
        if let Some(fresh) = entries.get(*cursor..) {
            retention.fold_all(fresh, entries);
            *cursor = entries.len();
        }
        *retention
    }

    /// Arm session cost telemetry (docs/transport.md): every recorded request
    /// spawns a fetch of its billed cost from this endpoint's accounting
    /// surface. Called by the runtime only when the provider has one
    /// (OpenRouter today); unarmed, `GET /state` reports `cost: null`.
    pub fn enable_cost_tracking(&self, endpoint: crate::inference::api::Endpoint) {
        self.cost.lock().unwrap().endpoint = Some(endpoint);
    }
}

/// Cap on a `/settle` (or `?wait`) long-poll: past this, return the current state regardless, so a
/// wedged loop can never hold a client forever. Far longer than any real turn — a clean settle wins
/// the race in practice; this is only the backstop.
pub(crate) const SETTLE_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) mod approvals;
pub(crate) mod artifact_links;
mod ask;
pub(crate) mod attachment_watch;
pub(crate) mod attachments;
pub(crate) mod attention;
mod background;
pub(crate) mod cold;
pub(crate) mod compact;
mod config_edits;
mod construct;
pub(crate) mod continuation;
mod delayed_push;
pub(crate) mod files_view;
mod identity;
pub(crate) mod keepalive;
mod metrics;
mod named_turns;
pub(crate) mod newer_version;
mod notifications;
mod operations;
mod pace;
mod paths;
mod person;
mod phase;
pub(crate) mod push;
pub(crate) mod review;
mod session_log;
mod settle;
mod stall;
pub(crate) mod status;
pub(crate) mod submitted;
mod subscriptions;
#[cfg(test)]
pub(crate) mod testing;
mod topic_values;
pub(crate) mod topics;
pub(crate) mod transcript;
mod verbs;
pub(crate) mod watch;

pub(crate) use person::PersonAction;
pub(crate) use watch::refresh_and_emit;

pub(crate) use approvals::Decider;
use approvals::Pending;

use config_edits::session_visible_notice;
pub(crate) use construct::SessionParts;
pub use metrics::{CacheRead, ColdStart, ColdStreak, USAGE_NAME, UsageLine, last_turn_usage};
use metrics::{CostCell, Metrics};
use notifications::Inbox;
use pace::Pace;
use topics::StateUpdate;

pub use keepalive::{believed_cold_since, keepalive_interval};
pub use paths::SessionPaths;
