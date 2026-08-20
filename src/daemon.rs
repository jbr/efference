//! The control channel — the daemon and its HTTP-over-Unix-socket interface
//! (docs/control-channel.md).
//!
//! This closes the interface-parity gap with a stance stronger than parity:
//! **the HTTP interface is the singular, authoritative input source.** The loop's
//! only [`Frontend`] is [`RemoteFrontend`], fed by the handlers here; there is no
//! privileged in-process path. Human (TUI), driver, and subagent are all clients
//! of the same socket.
//!
//! The thesis pays off (docs/README.md): the append-only log *is* the event store the
//! channel needs. `GET /log?since=<seq>` replays durably; ephemeral control state
//! (a pending approval) lives in [`Shared`] and is reported by `GET /state`. The
//! client's view is just another projection over the same log.
//!
//! Slice 1 (this file) is the daemon + write verbs + polled reads, drivable
//! headless over the socket. A pushed `GET /events` (SSE) and the in-process
//! client (`attach` + scriptable verbs) are the next slice.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_lite::Stream;
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use trillium::Conn;
use trillium_router::{Router, RouterConnExt};
use trillium_sse::{Eventable, sse};

use crate::agent;
use crate::api::{
    Client, Error, RequestUsage, ToolChoice, UpstreamFailureState, Usage, upstream_failure_state,
};

use crate::config::{ConfigSource, LoadedConfig, Refreshed};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend, Interrupt, SubmittedTurn};
use crate::log::{CallId, EditState, LogEntry, Payload, Seq, ToolCall, Turn};
use crate::persist::Log;
use crate::project::{ApiMessage, Retention, place_trailing_cache_breakpoint, project};
use crate::spotter::Spotter;
use crate::supervisor::{BgView, Supervisor};
use crate::tools::Toolbox;

/// A live, pre-commit output fragment streamed to clients as the control stream's
/// `output` class (docs/control-api.md). **Ephemeral and transport-only**:
/// never appended to the log, never projected, and droppable for a slow consumer.
/// The authoritative text always lands as a committed [`LogEntry`] on `/events`, so
/// the stream is a live convenience with the durable log as its backstop — which is
/// exactly why it can ride a lossy `broadcast` where `/events` must tail the log.
/// Every model-stream variant carries the content-block `index` it belongs to, and
/// `ToolCallStart` additionally the `tool_use` id: a fragment is partial delivery *of a
/// specific item*, and a client that cannot tell which holds one slot per kind and loses the
/// first tool call's arriving input the moment a second opens. [`ToolOutput`](Self::ToolOutput)
/// and [`ProcessOutput`](Self::ProcessOutput) were always keyed this way; these variants now
/// match them (docs/control-api.md).
#[derive(Clone)]
pub enum StreamDelta {
    /// A fragment of assistant prose as the model streams it from the upstream SSE.
    AssistantText { index: usize, text: String },
    /// A fragment of extended-thinking text as the model streams it. A separate variant from
    /// [`AssistantText`](Self::AssistantText) because it is *not* the assistant's answer — a client
    /// must render it as thinking or not at all, never as prose.
    ThinkingText { index: usize, text: String },
    /// The model began emitting a tool call, named before its arguments stream. Together with
    /// [`ThinkingText`](Self::ThinkingText) this is what makes a generation phase visible at all:
    /// before these existed only prose reached the stream, so a turn spent thinking or emitting a
    /// large tool input published nothing and read as a hung harness
    /// (docs/transport.md).
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    /// A fragment of a tool call's JSON input, verbatim. The bytes are passed through rather than
    /// summarized as a count: they are a partial slice of a JSON document, so a client must not
    /// *parse* them as if complete — but showing what actually arrived claims no structure, and
    /// `{"command":"cargo test` tells an operator far more than a byte count does. (Rendering a
    /// partial document *structurally* is a real option — `jiter`'s partial mode, `actson`'s push
    /// parser — and is filed as its own design question.)
    ToolCallInput { index: usize, fragment: String },
    /// A chunk of a running tool's combined output, tagged by its `tool_use` id.
    ToolOutput { call_id: String, chunk: String },
    /// A chunk of a running background process's output, tagged by the process's
    /// spawn-minted id (docs/supervisor.md; rendered `p#<n>` in prose, bare here —
    /// docs/supervisor.md). Demuxed by id exactly like
    /// [`ToolOutput`](Self::ToolOutput) is by `call_id`. Lossy: a cold-opened client
    /// seeds its tail from the process resource and layers these on top.
    ProcessOutput { id: u64, chunk: String },
}

/// Daemon state shared between the loop task and the HTTP handlers. Held as
/// `Arc<Shared>` in both the loop and trillium's server-global `shared_state`
/// (read in handlers via `conn.shared_state::<Arc<Shared>>()`). Per-conn state is
/// reserved for later multi-agent routing; one server-global `Shared` for now.
pub struct Shared {
    /// The loop appends under this lock (it no longer owns `Log` by value);
    /// handlers snapshot it for `GET /log`. The lock is only ever held across the
    /// synchronous `project`/append — never an `await`.
    log: Mutex<Log>,
    /// Sender half of the turn inbox; handlers push, the loop drains the receiver.
    turns_tx: mpsc::UnboundedSender<SubmittedTurn>,
    /// In-flight approval gates keyed by the `tool_use` `CallId` (already unique,
    /// already on the wire — no new id scheme).
    approvals: Mutex<HashMap<CallId, oneshot::Sender<Approval>>>,
    /// The single pending approval, surfaced by `GET /state` (the loop is
    /// sequential — one gate at a time, docs/README.md).
    pending: Mutex<Option<Pending>>,
    /// Fired by `POST /loop/interrupt`; the future `interrupted()` races a running tool.
    interrupt: Notify,
    /// The **latched** half of `POST /loop/interrupt` (docs/control-channel.md). `Notify::notify_waiters`
    /// wakes only a *currently registered* waiter and stores no permit, so during a call no backend
    /// awaits the abort future — every non-bash tool (`src/tools.rs`) — the wakeup has nowhere to
    /// land and is dropped. This flag is what survives it: the loop reads it at the junctures
    /// either side of a call and stops there, so the interrupt lands on tools it cannot suspend.
    /// Latching also closes the window between a bash spawn and the abort future's first poll.
    interrupt_latch: AtomicBool,
    /// Fired by `PATCH /processes/:id {background: true}` — the detach half of the interrupt intent
    /// (docs/supervisor.md, the foreground→background handoff). Like `interrupt`,
    /// `notify_waiters` reaches only a *currently-awaited* race, so a detach
    /// request with no foreground tool running is lost harmlessly.
    detach: Notify,
    /// The operator note optionally attached to a `PATCH /processes/:id {background: true}`
    /// (docs/supervisor.md): stashed by the handler, consumed by the loop only
    /// when a detach actually lands, and cleared when a fresh foreground bash
    /// starts so a request that never found a running tool can't mislabel a
    /// later handoff.
    detach_note: Mutex<Option<String>>,
    /// Pinged on each log append; the SSE tail (next slice) will wake on it.
    appended: Notify,
    /// Whether the loop is **executing a turn** (vs. parked awaiting one). Together with
    /// `queued`, this drives the `/settle` long-poll's "at a decision point?" predicate
    /// ([`Shared::is_settled`], docs/permissions.md). The loop owns it:
    /// set true when it dequeues a turn, false when it parks.
    running: AtomicBool,
    /// Turns submitted (`POST /turns`) but not yet dequeued by the loop — the submit→dequeue
    /// handoff counter that closes the cross-task race in [`Shared::is_settled`] (a turn exists
    /// even in the instant before the loop marks itself `running`).
    queued: AtomicUsize,
    /// Pinged whenever the daemon may have reached a **settled** state — a gate appeared
    /// ([`register_pending`]) or the loop parked ([`parking`]). The `/settle` long-poll waits on it.
    ///
    /// [`register_pending`]: Shared::register_pending
    /// [`parking`]: Shared::parking
    settle_changed: Notify,
    /// Pinged when a notification is enqueued (docs/inbox.md — `notify`). The agent loop's
    /// idle path awaits this to wake an idle agent for a standalone triage (the
    /// wake-on-notification model that replaced blocking `wait`).
    notified: Notify,
    /// Fan-out for ephemeral pre-commit output deltas (docs/README.md). Producers (the
    /// loop's `complete` sink and the bash tool) publish via [`Shared::publish_delta`];
    /// each control-stream connection wanting `output` frames subscribes. Lossy by
    /// design — see [`StreamDelta`].
    stream_tx: broadcast::Sender<StreamDelta>,
    /// Pinged when a state topic may have moved (docs/control-api.md). One notify for all
    /// ten topics, not one each: the reader recomputes everything and diffs, so a missed
    /// ping is repaired by the next one from any source rather than leaving a topic stale
    /// forever. See `topics.rs`.
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
    /// upstream request — origin-tagged, carrying the upstream response id — appended
    /// to the session dir's `usage.jsonl`. Transport-only, **never** the canonical
    /// log: usage must stay out of the projection. `None` (tests, offline) drops the
    /// lines; the in-memory `metrics` fold is unaffected.
    usage_log: Mutex<Option<io::BufWriter<std::fs::File>>>,
    /// The last non-success upstream response, shared with the HTTP client and
    /// surfaced by `GET /state`. Transport-only: never touches the log or projection
    /// (docs/transport.md).
    upstream_failure: UpstreamFailureState,
    /// Cumulative triage retention (docs/redaction-fork.md), folded lazily from the
    /// log behind a cursor rather than accumulated at the append sites. Unlike the
    /// transport telemetry above this is **derived from the canonical record**, so a
    /// resumed session reports the whole session's ratio instead of restarting at
    /// zero. The fold is monotone (a result's `wire` is fixed at append), so the
    /// cursor never needs invalidating — see [`Shared::retention`].
    retention: Mutex<(usize, Retention)>,
    /// The selected model's capabilities, fetched from the provider's metadata
    /// endpoint at boot (a required serve step — docs/transport.md) and set once
    /// via [`Shared::set_capabilities`]. `None` only in tests and offline verbs
    /// that never fetched; `GET /state` then reports `context: null`.
    /// Transport-only: never logged or projected.
    capabilities: Mutex<Option<crate::api::ModelCapabilities>>,
    /// Session cost telemetry (docs/transport.md): the running sum of
    /// per-generation **billed** costs fetched from the provider's accounting
    /// endpoint — authoritative, never modeled from a rate card. Behind its own
    /// `Arc` (like [`UpstreamFailureState`]) because the fetch runs as a spawned
    /// task after each recorded request. Armed only when the provider has a
    /// per-request cost surface (OpenRouter today); otherwise `GET /state`
    /// reports `cost: null`. Transport-only: never logged or projected.
    cost: Arc<Mutex<CostCell>>,

    /// The ephemeral inbox of pending ambient notifications (docs/inbox.md): out-of-band
    /// events awaiting their first triage gate. Control state, *not* a log payload —
    /// a notification only becomes a durable `Ambient` entry when a fork peek first
    /// shows it (Slice C). Superseded/dismissed-while-pending entries are dropped here
    /// (the "never-happened" case); only shown ones reach the audit log.
    inbox: Mutex<Inbox>,
    /// The pending-proposals queue: async `ConfigEdit`s (the session's `propose_scope`, the
    /// spotter's prefix-promotion) awaiting an orchestrator's commit/cancel
    /// (docs/permissions.md). Ephemeral control state like `inbox` — a
    /// `Proposed` entry is durable in the log, but its *pending* status lives here until resolved.
    proposals: Mutex<Vec<PendingProposal>>,
    /// The background-process registry (docs/supervisor.md): live children whose running tail
    /// is re-sampled fresh each triage gate. Ephemeral control state like `inbox` /
    /// `metrics` — never logged, never projected (a running tail forced through the
    /// immutable notification model would pile up stale snapshots). Pinned tails and
    /// exits *do* reach the durable log, but as `Ambient` notifications, not from here.
    supervisor: Mutex<Supervisor>,
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
    /// [`append`]: Shared::append
    effective: Mutex<Arc<crate::config::Config>>,
    /// The live config file — the format-preserving document currently on disk plus what it
    /// validates to (docs/permissions.md). `None` when the role resolved
    /// to the built-in default (no file to hold). Behind a lock because both the loop thread (a
    /// committed grant, and the check after an approved config write) and the fs-watch task reach
    /// it; every path in and out of the file goes through here, which is what keeps "the disk
    /// always holds the last content that parsed" true of *all* writers rather than some.
    config_file: Mutex<Option<crate::config::ConfigFile>>,
    /// The prefix-promotion probe ledger (docs/permissions.md): per-prefix
    /// seen-counts + probe votes, the state the descent reads to decide which prefix (if any) to ask
    /// the spotter to vouch for. Derived from the log's `Spot` entries (folded at startup via
    /// [`crate::policy::probe_ledger`], maintained incrementally as the loop assesses moves), never on
    /// the wire — like `effective`, behind a lock because the loop mutates it mid-session.
    probes: Mutex<crate::policy::ProbeLedger>,
    /// The operator's **review level** (docs/permissions.md), stored as an
    /// `AtomicU8` ordinal so it is lock-free and **runtime-mutable** (the `PATCH /loop {review_level}` verb,
    /// not just a boot flag). The loop's cascade reads it ([`Shared::review_level`]) to decide who
    /// reviews a gated call: `Autonomous` redirects the model instead of blocking on an absent
    /// human; `SpotterHuman` runs the spotter then the human gate; `HumanOnly` skips the spotter
    /// entirely and gates straight to the human. Orthogonal to pace. (The **config-write** gate is
    /// never flipped by this — a session's edit to its own config is always held for the
    /// orchestrator-above.)
    review_level: AtomicU8,
    /// The operator's **pace** dial (docs/control-channel.md): a between-turns
    /// wall-clock control that is *transparent to the agent*. Pure operator-view state — never a
    /// log entry, never projected — so a held or slowed loop is byte-identical on the wire to a
    /// fast one (that, plus timestamps-never-projected, is the invisibility guarantee). Read at the
    /// seam by [`Shared::pace_at_seam`].
    pace: Mutex<Pace>,
    /// Wakes an in-flight [`pace_at_seam`](Shared::pace_at_seam) sleep so a release or a submitted
    /// turn ends the wait *immediately* rather than after the remaining delay. Fired by
    /// [`submit_turn`](Shared::submit_turn) (submit ⇒ resume, always) and by a release
    /// (`set_pace` dropping `held`); a pure speed change or a *hold* does not fire it (those take
    /// effect at the next juncture).
    pace_interrupt: Notify,
    /// The session scratch dir (docs/tools.md): minted by `serve`, exported to bash
    /// children as `$TMPDIR` (foreground via the toolbox's `Bash`, background via
    /// [`spawn_background`](Self::spawn_background)), granted rw in the injected
    /// path stack, and the one value the structured-tool expander resolves. All
    /// projections of this one value. `None` in tests and non-executing contexts.
    scratch: Option<PathBuf>,
    /// The frozen env map for structured-tool path expansion
    /// ([`crate::expand`]): `{TMPDIR: scratch}` when a scratch dir exists, else
    /// empty — every `$NAME` then errors, which is the fail-closed default.
    expander: crate::expand::Expander,
}

impl Shared {
    /// Build the shared state under the built-in default role (no config). The real
    /// daemon uses [`Shared::with_role`]; this convenience keeps the many tests that
    /// don't exercise role config terse.
    pub fn new(log: Log) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role(log, LoadedConfig::default())
    }

    /// Build the shared state under a resolved role and hand back the turn-inbox
    /// receiver for the loop.
    pub fn with_role(
        log: Log,
        role: LoadedConfig,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role_and_scratch(log, role, None)
    }

    /// The full constructor: [`with_role`](Self::with_role) plus the session
    /// scratch dir (docs/tools.md), whose value seeds the structured-tool path
    /// expander and the background spawn env.
    pub fn with_role_and_scratch(
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role_scratch_and_upstream_failure(log, role, scratch, upstream_failure_state())
    }

    /// The daemon constructor with the upstream client's shared transport
    /// diagnostic (docs/transport.md). Tests normally use the public convenience
    /// constructors above, which allocate an empty cell.
    pub(crate) fn with_role_scratch_and_upstream_failure(
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
        upstream_failure: UpstreamFailureState,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        let (turns_tx, turns_rx) = mpsc::unbounded_channel();

        // Bounded ring: a consumer that falls this far behind drops the oldest
        // deltas (a `Lagged` on `recv`) rather than stalling the loop — the log is
        // the durable record, so a dropped delta is only a momentary gap in the
        // live view.
        let (stream_tx, _) = broadcast::channel(1024);
        // State updates are whole-topic replacements, so a lagging connection loses
        // nothing it cannot recover: the newest value for each topic is the only one that
        // matters, and a `Lagged` is resolved by re-reading the topics rather than by
        // replay. Sized well above the topic count so that is rare rather than routine.
        let (state_tx, _) = broadcast::channel(256);
        // Fold any grants already in the log (a resumed session) into the initial effective
        // role; from here it's maintained incrementally on append. Computed before `log` is
        // moved into the struct.
        let effective = Arc::new(crate::policy::effective_config(&role.role, log.entries()));
        // Fold the probe ledger from a resumed log (no-op for a fresh one), then maintain it
        // incrementally as the loop assesses moves (docs/permissions.md).
        let probes = crate::policy::probe_ledger(log.entries());
        // Seed the process-id mint past every `p#<n>` a resumed log mentions, so a restarted
        // daemon never re-mints an ordinal the durable record already refers to
        // (docs/supervisor.md). Scanned over the serialized entry because
        // handles land in prose (tool results, notification sources), not in a typed field.
        let minted = log
            .entries()
            .iter()
            .map(|e| {
                serde_json::to_string(e).map_or(0, |s| crate::supervisor::max_handle_mention(&s))
            })
            .max()
            .unwrap_or(0);
        // Open the file the role resolved from, if any. Boot has already loaded and validated it,
        // so this parses; a failure here is reported and leaves the session file-less (edits stay
        // in the log fold) rather than aborting a session over a race with an external writer.
        let config_file = match &role.source {
            ConfigSource::Path(path) => match crate::config::ConfigFile::load(path) {
                Ok(file) => Some(file),
                Err(e) => {
                    eprintln!(
                        "[config: could not open {} for editing: {e}]",
                        path.display()
                    );
                    None
                }
            },
            ConfigSource::Default => None,
        };
        let shared = Arc::new(Self {
            log: Mutex::new(log),
            turns_tx,
            approvals: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            interrupt: Notify::new(),
            interrupt_latch: AtomicBool::new(false),
            detach: Notify::new(),
            detach_note: Mutex::new(None),
            appended: Notify::new(),
            running: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            settle_changed: Notify::new(),
            notified: Notify::new(),
            stream_tx,
            state_changed: Notify::new(),
            state_tx,
            metrics: Mutex::new(Metrics::default()),
            usage_log: Mutex::new(None),
            upstream_failure,
            retention: Mutex::new((0, Retention::default())),
            capabilities: Mutex::new(None),
            cost: Arc::new(Mutex::new(CostCell::default())),
            inbox: Mutex::new(Inbox::default()),

            proposals: Mutex::new(Vec::new()),
            supervisor: Mutex::new(Supervisor::with_minted(minted)),
            role,
            effective: Mutex::new(effective),
            config_file: Mutex::new(config_file),
            probes: Mutex::new(probes),
            review_level: AtomicU8::new(crate::policy::ReviewLevel::SpotterHuman.as_u8()),
            pace: Mutex::new(Pace::default()),
            pace_interrupt: Notify::new(),
            expander: crate::expand::Expander::new(
                scratch
                    .iter()
                    .map(|p| ("TMPDIR".to_string(), p.to_string_lossy().into_owned()))
                    .collect(),
            ),
            scratch,
        });
        (shared, turns_rx)
    }

    /// Store the boot-fetched model capabilities (docs/transport.md). Called once
    /// by the daemon's `serve` before the loop starts; the value is immutable
    /// thereafter and read by `GET /state`'s context readout.
    pub fn set_capabilities(&self, capabilities: crate::api::ModelCapabilities) {
        *self.capabilities.lock().unwrap() = Some(capabilities);
    }

    /// Cumulative triage retention over the whole session (docs/redaction-fork.md),
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
            retention.fold_all(fresh);
            *cursor = entries.len();
        }
        *retention
    }

    /// Arm session cost telemetry (docs/transport.md): every recorded request
    /// spawns a fetch of its billed cost from this endpoint's accounting
    /// surface. Called by the daemon's `serve` only when the provider has one
    /// (OpenRouter today); unarmed, `GET /state` reports `cost: null`.
    pub fn enable_cost_tracking(&self, endpoint: crate::api::Endpoint) {
        self.cost.lock().unwrap().endpoint = Some(endpoint);
    }

    /// Resolve `$TMPDIR` in a structured call's path fields against the frozen
    /// session env map ([`crate::expand`]) — the loop's one expansion choke
    /// point, upstream of classify/spot/exec. `Ok(None)` = untouched.
    pub fn expand_call(
        &self,
        call: &crate::log::ToolCall,
    ) -> Result<Option<crate::log::ToolCall>, String> {
        crate::expand::expand_call(&self.expander, call)
    }

    /// [`append`](Self::append) carrying the submission id of the
    /// `POST /v1/turns` the entry realizes (docs/control-api.md) — the loop's
    /// user-turn appends route here so the entry names its originating
    /// submission.
    pub fn append_submission(
        &self,
        turn: Turn,
        payload: Payload,
        submission_id: String,
    ) -> io::Result<Seq> {
        // User turns only — a submitted turn is never a `ConfigEdit`, so none of
        // `append`'s proposal-lifecycle handling applies here.
        let seq = self
            .log
            .lock()
            .unwrap()
            .append_submission(turn, payload, Some(submission_id))?;
        self.appended.notify_waiters();
        Ok(seq)
    }

    /// Append to the log (durably) and wake SSE tailers. The single appender is
    /// the loop; handlers only read.
    pub fn append(&self, turn: Turn, payload: Payload) -> io::Result<Seq> {
        // A `ConfigEdit` drives the proposal lifecycle (docs/permissions.md):
        // capture its `(state, change)` before the payload moves into the log, then act on it
        // *after* a successful append, so a failed write never edits state. Cheap clone, edits only.
        let edit = match &payload {
            Payload::ConfigEdit(edit) => Some((edit.state, edit.change.clone(), edit.resolves)),
            _ => None,
        };
        let seq = self.log.lock().unwrap().append(turn, payload)?;
        if let Some((state, change, resolves)) = edit {
            match state {
                // A committed edit takes effect: fold it into the effective policy so a gate reads
                // current policy in O(1) (no log re-scan), and — for session-visible keys — notify.
                EditState::Committed => {
                    {
                        let mut eff = self.effective.lock().unwrap();
                        let mut role = (**eff).clone();
                        crate::policy::apply_config_change(&mut role, &change);
                        *eff = Arc::new(role);
                    }
                    // Resolving a proposal: drop the referenced proposal from the pending queue.
                    // `None` for a direct synchronous/fs-watch commit, which had no queued proposal.
                    if let Some(proposal) = resolves {
                        self.resolve_proposal(proposal);
                    }
                    // Session-visible edits (`scope`, `paths`) surface as an `Ambient` notification
                    // so the session learns its perimeter moved — an optional tail append that never
                    // re-renders the cached preamble (docs/permissions.md). It rides
                    // the same inbox/triage machinery as any notification, and fires for *every*
                    // committing initiator because this is the single appender. `commands`/`tools`
                    // notify nothing — their effect reaches the model only through the classifier.
                    if let Some((body, slot)) = session_visible_notice(&change) {
                        self.notify("config".to_string(), body, Some(slot), None);
                    }
                }
                // A proposal is inert in the effective config: it joins the pending-proposals queue,
                // addressed by its own `seq`, awaiting an orchestrator's commit/cancel.
                EditState::Proposed => {
                    self.proposals
                        .lock()
                        .unwrap()
                        .push(PendingProposal { seq, change });
                }
                // A cancel drops the referenced proposal from the queue; the effective config
                // never moved.
                EditState::Cancelled => {
                    if let Some(proposal) = resolves {
                        self.resolve_proposal(proposal);
                    }
                }
            }
        }
        self.appended.notify_waiters();
        Ok(seq)
    }

    /// Publish an ephemeral pre-commit output delta to the control stream's `output`
    /// subscribers (docs/control-api.md). Intentionally lossy: a send with no
    /// subscribers — or to a lagging one — is dropped, because the committed entry on
    /// the `log` class is the authoritative copy. Never touches the log.
    pub fn publish_delta(&self, delta: StreamDelta) {
        let _ = self.stream_tx.send(delta);
    }

    /// A fresh subscription to the delta fan-out for one control-stream connection.
    fn subscribe_stream(&self) -> broadcast::Receiver<StreamDelta> {
        self.stream_tx.subscribe()
    }

    /// Project the current log into the wire (docs/README.md). Lock held only for the
    /// synchronous fold.
    pub fn project_messages(&self) -> Vec<ApiMessage> {
        project(self.log.lock().unwrap().entries())
    }

    /// A clone of the entries — handed to the fork distiller, which awaits the
    /// model and so can't borrow across the lock (docs/redaction-fork.md).
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.log.lock().unwrap().entries().to_vec()
    }

    /// The turn of the most recent log entry (`0` if empty) — the turn an **out-of-band** append
    /// attaches to (the fs-watch initiator, which has no loop-turn context,
    /// docs/permissions.md). A `ConfigEdit` projects to nothing, so its turn
    /// is pure audit grouping; attaching it to the in-flight turn is the honest placement.
    fn latest_turn(&self) -> Turn {
        self.log
            .lock()
            .unwrap()
            .entries()
            .last()
            .map(|e| e.turn)
            .unwrap_or(0)
    }

    /// A clone of the single entry at `seq` (== its index in the log), or `None`
    /// if the log hasn't reached it yet. The `/events` tail reads one entry at a
    /// time, so it never clones the whole log.
    fn entry_at(&self, seq: u64) -> Option<LogEntry> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .get(seq as usize)
            .cloned()
    }
}

/// Cap on a `/settle` (or `?wait`) long-poll: past this, return the current state regardless, so a
/// wedged loop can never hold a client forever. Far longer than any real turn — a clean settle wins
/// the race in practice; this is only the backstop.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(300);

mod approvals;
mod background;
mod config_edits;
// The typed error envelope (docs/control-api.md) — every non-2xx body the router
// answers with. `GateMoved` is still unconstructed: it belongs to the
// resource-renamed gate.
#[allow(dead_code)]
mod errors;
// The control-stream frame envelope (docs/control-api.md), defined ahead of its
// construction sites, like `WireState`'s variants were: pinning the contract first is
// what keeps adding the unified endpoint from also reshaping it. `dead_code` until the
// stream emits frames.
#[allow(dead_code)]
mod frames;
mod http;
mod keepalive;
mod metrics;
mod notifications;
mod pace;
mod stream;
mod tcp;
mod topics;
mod views;
mod watch;

pub(crate) use watch::refresh_and_emit;

use approvals::Pending;
use config_edits::{PendingProposal, session_visible_notice};
use errors::{ErrorBody, ErrorCode, fail, respond};
use frames::{Frame, Topic};
pub use metrics::RequestOrigin;
use metrics::{CostCell, Metrics};
use notifications::Inbox;
use pace::Pace;
use stream::{Include, control_stream};
use topics::{StateUpdate, watch_topics};

use http::{LogSelect, router};
use keepalive::keepalive_loop;
use watch::config_watch_loop;

#[cfg(test)]
mod test;

mod frontend;
mod serve;

pub use frontend::RemoteFrontend;
pub use serve::serve;
pub use tcp::{TcpBind, TlsPem};
