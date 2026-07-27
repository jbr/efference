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
use trillium_sse::{Eventable, SseConnExt};

use crate::agent;
use crate::api::{Client, Error, ToolChoice, Usage};
use crate::config::{ConfigSource, LoadedConfig, Refreshed};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend, Interrupt};
use crate::log::{CallId, EditState, LogEntry, Payload, Seq, ToolCall, Turn};
use crate::persist::Log;
use crate::project::{ApiMessage, place_trailing_cache_breakpoint, project};
use crate::spotter::Spotter;
use crate::supervisor::{BgView, Supervisor};
use crate::tools::Toolbox;

/// A live, pre-commit output fragment streamed to clients over `GET /stream`
/// (docs/README.md — the deferred token-level deltas). **Ephemeral and transport-only**:
/// never appended to the log, never projected, and droppable for a slow consumer.
/// The authoritative text always lands as a committed [`LogEntry`] on `/events`, so
/// the stream is a live convenience with the durable log as its backstop — which is
/// exactly why it can ride a lossy `broadcast` where `/events` must tail the log.
#[derive(Clone)]
pub enum StreamDelta {
    /// A fragment of assistant prose as the model streams it from the upstream SSE.
    AssistantText(String),
    /// A fragment of extended-thinking text as the model streams it. A separate variant from
    /// [`AssistantText`](Self::AssistantText) because it is *not* the assistant's answer — a client
    /// must render it as thinking or not at all, never as prose.
    ThinkingText(String),
    /// The model began emitting a tool call, named before its arguments stream. Together with
    /// [`ThinkingText`](Self::ThinkingText) this is what makes a generation phase visible at all:
    /// before these existed only prose reached the stream, so a turn spent thinking or emitting a
    /// large tool input published nothing and read as a hung harness
    /// (backlog/dogfooding-friction.md).
    ToolCallStart { name: String },
    /// A fragment of a tool call's JSON input, verbatim. The bytes are passed through rather than
    /// summarized as a count: they are a partial slice of a JSON document, so a client must not
    /// *parse* them as if complete — but showing what actually arrived claims no structure, and
    /// `{"command":"cargo test` tells an operator far more than a byte count does. (Rendering a
    /// partial document *structurally* is a real option — `jiter`'s partial mode, `actson`'s push
    /// parser — and is filed as its own design question.)
    ToolCallInput { fragment: String },
    /// A chunk of a running tool's combined output, tagged by its `tool_use` id.
    ToolOutput { call_id: String, chunk: String },
    /// A chunk of a running background process's output, tagged by its `bg#<n>`
    /// handle (docs/supervisor.md, TUI-bg-2). Demuxed by handle exactly like
    /// [`ToolOutput`](Self::ToolOutput) is by `call_id` — it smooths the live process
    /// panel's tail between the ~400ms `/state` polls. The poll snapshot stays the
    /// authoritative tail; these just fill the gaps.
    BgOutput { handle: String, chunk: String },
}

/// An in-flight approval surfaced to clients via `GET /state` (the ephemeral
/// control state the log doesn't record — it stores only the *result*).
#[derive(Clone, Serialize)]
struct Pending {
    call_id: String,
    name: String,
    input: Value,
    /// The spotter's one-line reason for routing this call to the human, if it gated via the
    /// spotter (docs/permissions.md) — read from the `Spot` audit entry the spotter logs *before*
    /// the gate. Lets the client show *why* a call is held without a new side channel. `None` when
    /// no spotter verdict preceded the gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// The pending-notification queue + its id counter (docs/inbox.md). Ids are
/// **queue-local**: an un-shown notification has no log `seq` yet, so it's addressed
/// by this id until Slice C's first-show append gives it a durable `seq`.
#[derive(Default)]
struct Inbox {
    next_id: u64,
    pending: Vec<PendingNotification>,
}

/// One pending ambient notification, surfaced by `GET /state` (docs/inbox.md). Immutable
/// once enqueued — supersession archives-and-replaces rather than editing (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize)]
struct PendingNotification {
    /// Queue-local id; the handle `POST /notifications/:id/dismiss` addresses.
    id: u64,
    source: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
}

/// One async config edit awaiting an orchestrator decision
/// (docs/permissions.md). Filed by a `Proposed` [`ConfigEdit`] append (the
/// session's `propose_scope`, the spotter's prefix-promotion); the orchestrator-above commits or
/// cancels it, which drops it from the queue. Unlike [`PendingNotification`], the proposal **already
/// has a durable `seq`** (the `Proposed` entry's), so that *is* its id — no queue-local counter.
#[derive(Clone, Debug, Serialize)]
struct PendingProposal {
    /// The `seq` of the `Proposed` log entry — the proposal's id, which the orchestrator's
    /// commit/cancel references.
    seq: Seq,
    /// The change being proposed; the committing entry copies it self-contained so the fold stays a
    /// pure per-entry filter (docs/permissions.md).
    change: crate::log::ConfigChange,
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
    turns_tx: mpsc::UnboundedSender<String>,
    /// In-flight approval gates keyed by the `tool_use` `CallId` (already unique,
    /// already on the wire — no new id scheme).
    approvals: Mutex<HashMap<CallId, oneshot::Sender<Approval>>>,
    /// The single pending approval, surfaced by `GET /state` (the loop is
    /// sequential — one gate at a time, docs/README.md).
    pending: Mutex<Option<Pending>>,
    /// Fired by `POST /interrupt`; the future `interrupted()` races a running tool.
    interrupt: Notify,
    /// The **latched** half of `POST /interrupt` (docs/control-channel.md). `Notify::notify_waiters`
    /// wakes only a *currently registered* waiter and stores no permit, so during a call no backend
    /// awaits the abort future — every non-bash tool (`src/tools.rs`) — the wakeup has nowhere to
    /// land and is dropped. This flag is what survives it: the loop reads it at the junctures
    /// either side of a call and stops there, so the interrupt lands on tools it cannot suspend.
    /// Latching also closes the window between a bash spawn and the abort future's first poll.
    interrupt_latch: AtomicBool,
    /// Fired by `POST /background` — the detach half of the interrupt intent
    /// (docs/supervisor.md, the foreground→background handoff). Like `interrupt`,
    /// `notify_waiters` reaches only a *currently-awaited* race, so a detach
    /// request with no foreground tool running is lost harmlessly.
    detach: Notify,
    /// The operator note optionally attached to a `POST /background`
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
    /// Turns submitted (`POST /turn`) but not yet dequeued by the loop — the submit→dequeue
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
    /// each `GET /stream` connection subscribes. Lossy by design — see [`StreamDelta`].
    stream_tx: broadcast::Sender<StreamDelta>,
    /// Cache/usage telemetry — the most recent request's `usage` and a wall-clock
    /// stamp, surfaced by `GET /state` so clients can see cache hit/miss and
    /// predict TTL eviction (docs/log-and-projection.md). Transport-only: never touches the log.
    metrics: Mutex<Metrics>,
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
    /// `AtomicU8` ordinal so it is lock-free and **runtime-mutable** (the `POST /review-level` verb,
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

/// The operator's pacing state (docs/control-channel.md): a continuous speed dial
/// plus a pause **overlay**. `effective_sleep = held ? ∞ : speed` — `held` is an overlay, *not* a
/// speed value, so the dial retains `speed` while held and releasing returns to the retained speed,
/// never to zero. Freely adjustable while held and outliving it, so a change made during a pause
/// "recolors" what resume does.
#[derive(Clone, Copy, Default)]
struct Pace {
    /// The between-turns delay when not held (`0` = as fast as possible, the default).
    speed: Duration,
    /// The pause overlay: while set, the effective sleep is infinite (wait at the juncture until
    /// released or a turn is submitted). Retains `speed` underneath.
    held: bool,
}

/// Per-session cache/usage telemetry. `last_request_at` is an [`Instant`] (not
/// serialized — we surface the derived elapsed seconds, so the non-determinism
/// never reaches a client and there's nothing to keep byte-stable).
#[derive(Default)]
struct Metrics {
    last_usage: Option<Usage>,
    /// Stamped by **any** request — real turn, fork peek, or keepalive ping. The
    /// cache-touch clock: drives `seconds_since_last_request` and when to ping.
    last_request_at: Option<Instant>,
    /// Stamped by real loop turns (incl. the fork) but **not** keepalive pings, so
    /// it measures genuine idle and a ping can't reset its own deadline. Gates the
    /// keepalive cap (docs/log-and-projection.md).
    last_real_turn_at: Option<Instant>,
    total_input: u64,
    total_output: u64,
    total_cache_read: u64,
    total_cache_creation: u64,
}

impl Shared {
    /// Build the shared state under the built-in default role (no config). The real
    /// daemon uses [`Shared::with_role`]; this convenience keeps the many tests that
    /// don't exercise role config terse.
    pub fn new(log: Log) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        Self::with_role(log, LoadedConfig::default())
    }

    /// Build the shared state under a resolved role and hand back the turn-inbox
    /// receiver for the loop.
    pub fn with_role(log: Log, role: LoadedConfig) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        Self::with_role_and_scratch(log, role, None)
    }

    /// The full constructor: [`with_role`](Self::with_role) plus the session
    /// scratch dir (docs/tools.md), whose value seeds the structured-tool path
    /// expander and the background spawn env.
    pub fn with_role_and_scratch(
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (turns_tx, turns_rx) = mpsc::unbounded_channel();
        // Bounded ring: a consumer that falls this far behind drops the oldest
        // deltas (a `Lagged` on `recv`) rather than stalling the loop — the log is
        // the durable record, so a dropped delta is only a momentary gap in the
        // live view.
        let (stream_tx, _) = broadcast::channel(1024);
        // Fold any grants already in the log (a resumed session) into the initial effective
        // role; from here it's maintained incrementally on append. Computed before `log` is
        // moved into the struct.
        let effective = Arc::new(crate::policy::effective_config(&role.role, log.entries()));
        // Fold the probe ledger from a resumed log (no-op for a fresh one), then maintain it
        // incrementally as the loop assesses moves (docs/permissions.md).
        let probes = crate::policy::probe_ledger(log.entries());
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
            metrics: Mutex::new(Metrics::default()),
            inbox: Mutex::new(Inbox::default()),
            proposals: Mutex::new(Vec::new()),
            supervisor: Mutex::new(Supervisor::new()),
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

    /// Resolve `$TMPDIR` in a structured call's path fields against the frozen
    /// session env map ([`crate::expand`]) — the loop's one expansion choke
    /// point, upstream of classify/spot/exec. `Ok(None)` = untouched.
    pub fn expand_call(
        &self,
        call: &crate::log::ToolCall,
    ) -> Result<Option<crate::log::ToolCall>, String> {
        crate::expand::expand_call(&self.expander, call)
    }

    /// Record a completed request's `usage` and stamp the wall clock. The loop
    /// calls this once per API round-trip; `GET /state` reports the latest plus
    /// running totals and the seconds-since (the eviction-prediction signal,
    /// docs/log-and-projection.md). Transport-only — no log append, no projection change.
    pub fn record_request(&self, usage: Usage) {
        let mut m = self.metrics.lock().unwrap();
        Self::fold_usage(&mut m, usage);
        m.last_real_turn_at = Some(Instant::now());
    }

    /// A keepalive ping's usage (docs/log-and-projection.md). Folded into the telemetry and the
    /// cache-touch clock like any request, but it does **not** stamp the real-turn
    /// clock — a ping must not reset the cap that decides when to stop pinging.
    pub fn record_keepalive(&self, usage: Usage) {
        let mut m = self.metrics.lock().unwrap();
        Self::fold_usage(&mut m, usage);
    }

    fn fold_usage(m: &mut Metrics, usage: Usage) {
        m.total_input += usage.input_tokens;
        m.total_output += usage.output_tokens;
        m.total_cache_read += usage.cache_read_input_tokens;
        m.total_cache_creation += usage.cache_creation_input_tokens;
        m.last_request_at = Some(Instant::now());
        m.last_usage = Some(usage);
    }

    /// `(since last cache-touch, since last real turn)` — the keepalive task's two
    /// clocks. The first decides *when* to ping (the cache is nearing its TTL); the
    /// second enforces the *cap* (a ping doesn't reset it, so unbounded idle still
    /// stops). Both `None` until the first real turn — nothing to keep warm yet.
    fn keepalive_clocks(&self) -> (Option<Duration>, Option<Duration>) {
        let m = self.metrics.lock().unwrap();
        (
            m.last_request_at.map(|t| t.elapsed()),
            m.last_real_turn_at.map(|t| t.elapsed()),
        )
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

    /// Drop a pending proposal from the queue once an orchestrator resolves it (commit or cancel).
    /// The committing/cancelling entry references the proposal by its `seq`; a direct commit (the
    /// synchronous human gate, fs-watch) has no queued proposal, so this is a no-op there.
    fn resolve_proposal(&self, seq: Seq) {
        self.proposals.lock().unwrap().retain(|p| p.seq != seq);
    }

    /// The current pending-proposals queue — async config edits awaiting an orchestrator decision
    /// (docs/permissions.md). Surfaced to the orchestrator-above on `GET
    /// /proposals` and in `/state`, and consumed by the commit/cancel verbs.
    fn pending_proposals(&self) -> Vec<PendingProposal> {
        self.proposals.lock().unwrap().clone()
    }

    /// Resolve a pending proposal by `seq` — the orchestrator-above's commit/cancel decision over
    /// `POST /proposals/:seq` (docs/permissions.md). **Commit** appends a
    /// `Committed` [`ConfigEdit`] carrying the proposal's `change`, which `append`'s fold applies to
    /// the effective config (and notifies the session for `scope`/`paths`); **cancel** appends a
    /// `Cancelled` entry (audit-only). Both reference the proposal by `seq`, so the fold drops it
    /// from the pending queue. Returns `false` if no pending proposal has that `seq`. The commit is
    /// session-effective — not written back to the config file (that persistence is a later
    /// refinement, like the gate's persist option).
    fn resolve_proposal_decision(&self, seq: Seq, commit: bool, note: String) -> io::Result<bool> {
        let Some(change) = self
            .proposals
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.seq == seq)
            .map(|p| p.change.clone())
        else {
            return Ok(false);
        };
        let edit = crate::log::ConfigEdit {
            state: if commit {
                EditState::Committed
            } else {
                EditState::Cancelled
            },
            resolves: Some(seq),
            change,
            note,
        };
        self.append(self.latest_turn(), Payload::ConfigEdit(edit))?;
        Ok(true)
    }

    /// Publish an ephemeral pre-commit output delta to `GET /stream` subscribers
    /// (docs/README.md). Intentionally lossy: a send with no subscribers — or to a
    /// lagging one — is dropped, because the committed entry on `/events` is the
    /// authoritative copy. Never touches the log.
    pub fn publish_delta(&self, delta: StreamDelta) {
        let _ = self.stream_tx.send(delta);
    }

    /// A fresh subscription to the delta fan-out for one `GET /stream` connection.
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

    /// The resolved role's policy, for the loop's fast-path classifier
    /// (docs/permissions.md). Immutable for the session, so a borrow
    /// is fine — no lock.
    pub fn role(&self) -> &crate::config::Config {
        &self.role.role
    }

    /// The effective policy the cascade should consult: base role + the fold of every grant
    /// appended so far (docs/permissions.md). Returns an `Arc` clone of the
    /// incrementally-maintained role — O(1), and safe to hold across the spotter's await (a
    /// lock guard would not be). Falls back to the base if no grants have landed.
    pub fn effective_config(&self) -> Arc<crate::config::Config> {
        self.effective.lock().unwrap().clone()
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

    /// The role config file path, if the role resolved from one (`None` for the built-in
    /// default — nothing to persist to). Where a `Lifetime::Persist` grant is appended
    /// (docs/permissions.md).
    pub fn config_path(&self) -> Option<&std::path::Path> {
        match &self.role.source {
            ConfigSource::Path(p) => Some(p),
            ConfigSource::Default => None,
        }
    }

    /// Edit a committed [`ConfigChange`] into the config file, preserving its formatting
    /// (docs/permissions.md). Every committed grant goes through here, so the file
    /// stays a complete record of the perimeter the session ran under — the artifact an operator
    /// reads, and copies grants up to the origin template from. A session with no config file (the
    /// built-in default role) is a no-op: there is no file to diverge from.
    pub fn apply_config_change(&self, change: &crate::log::ConfigChange) -> std::io::Result<()> {
        match self.config_file.lock().unwrap().as_mut() {
            Some(file) => file.apply(change),
            None => Ok(()),
        }
    }

    /// Re-read the config file, adopting it if it parses and reverting it if it does not
    /// (docs/permissions.md). `None` when there's no file. Both callers
    /// — the fs-watch task and the loop, right after an approved config write — go through here,
    /// because the invariant is about the file's state, not about who edited it.
    pub fn refresh_config(&self) -> Option<crate::config::Refreshed> {
        self.config_file
            .lock()
            .unwrap()
            .as_mut()
            .map(|f| f.refresh())
    }

    /// The current **review level** (docs/permissions.md) — who reviews a
    /// gated call. Runtime-mutable (the `POST /review-level` verb); read by the loop's cascade and
    /// reported on `GET /state`. Relaxed ordering: a lone dial with no happens-before tie to other
    /// state, read on the loop thread and written by a handler thread.
    pub fn review_level(&self) -> crate::policy::ReviewLevel {
        crate::policy::ReviewLevel::from_u8(self.review_level.load(Ordering::Relaxed))
    }

    /// Set the review level — the runtime `POST /review-level` verb, or `serve --autonomous` at
    /// boot via [`set_autonomous`](Self::set_autonomous).
    pub fn set_review_level(&self, level: crate::policy::ReviewLevel) {
        self.review_level.store(level.as_u8(), Ordering::Relaxed);
    }

    /// Whether no synchronous human sits at the gate — derived from the review level
    /// (`Autonomous`). Kept for the `GET /state` `autonomous` field and callers that only care
    /// about the redirect-vs-block distinction.
    pub fn autonomous(&self) -> bool {
        self.review_level() == crate::policy::ReviewLevel::Autonomous
    }

    /// Set autonomous mode at boot (`serve --autonomous`): maps to the `Autonomous` review level,
    /// else the supervised default (`SpotterHuman`).
    pub fn set_autonomous(&self, on: bool) {
        self.set_review_level(if on {
            crate::policy::ReviewLevel::Autonomous
        } else {
            crate::policy::ReviewLevel::SpotterHuman
        });
    }

    /// Observe one spotter-assessed bash command and pick the prefix to probe for promotion, if any
    /// (docs/permissions.md). Bumps the seen-counts for the command's token-prefixes,
    /// then runs the descent against the current effective policy — one lock acquisition.
    pub fn observe_and_pick_probe(
        &self,
        role: &crate::config::Config,
        tokens: &[String],
    ) -> Option<String> {
        let mut probes = self.probes.lock().unwrap();
        probes.observe(tokens);
        probes.next_probe(role, tokens)
    }

    /// Record a probe's vote (`safe` iff the spotter answered `clear_always`) and report whether the
    /// prefix has now reached the promotion threshold `K` — the loop's signal to file a
    /// `Proposed(Command)` (docs/permissions.md).
    pub fn record_probe_vote(&self, prefix: &str, safe: bool) -> bool {
        let mut probes = self.probes.lock().unwrap();
        probes.vote(prefix, safe);
        probes.ready_to_promote(prefix)
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

    fn submit_turn(&self, text: String) {
        // Count the turn as queued *before* sending, so `is_settled` reads "busy" the instant
        // the turn is accepted — even before the loop dequeues it (the `/settle` submit race,
        // docs/permissions.md).
        self.queued.fetch_add(1, Ordering::SeqCst);
        // Submit ⇒ resume, always (docs/control-channel.md): drop any hold and wake an
        // in-flight juncture sleep so the message is delivered at the next juncture *immediately*, not
        // after the remaining paced delay. The retained `speed` is untouched (it recolors later
        // junctures); only the `held` overlay drops.
        {
            let mut p = self.pace.lock().unwrap();
            p.held = false;
        }
        self.pace_interrupt.notify_waiters();
        // The receiver lives in the loop's `RemoteFrontend`; a send fails only if
        // the loop has exited, in which case the daemon is shutting down anyway.
        let _ = self.turns_tx.send(text);
    }

    /// The current pace `(speed, held)` — surfaced on `GET /state` and read by the juncture sleep.
    fn pace_snapshot(&self) -> (Duration, bool) {
        let p = self.pace.lock().unwrap();
        (p.speed, p.held)
    }

    /// Apply an operator pace change (docs/control-channel.md — the `POST /pace` verb):
    /// set the `speed` dial and/or the `held` overlay, each optional. **Releasing** a hold (`held`
    /// true → false) wakes an in-flight juncture sleep so resume is immediate; a pure speed change or a
    /// *hold* does not wake it — those take effect at the next juncture, per the design (a hold lands at
    /// the next juncture, letting the in-flight step finish; a speed change outlives the hold and
    /// recolors resume). Returns the resulting `(speed, held)`.
    fn set_pace(&self, speed: Option<Duration>, held: Option<bool>) -> (Duration, bool) {
        let mut wake = false;
        let result = {
            let mut p = self.pace.lock().unwrap();
            if let Some(s) = speed {
                p.speed = s;
            }
            if let Some(h) = held {
                if p.held && !h {
                    wake = true; // releasing a hold ends the juncture sleep now
                }
                p.held = h;
            }
            (p.speed, p.held)
        };
        if wake {
            self.pace_interrupt.notify_waiters();
        }
        result
    }

    /// The interruptible between-turns sleep (docs/control-channel.md). Applied at the
    /// juncture — after a step's results land, before the next model call — and **transparent to the
    /// agent**: it touches no log entry, so a held/slowed loop projects byte-identically to a fast
    /// one. Waits `effective_sleep = held ? ∞ : speed`, but returns *immediately* if a turn is
    /// already queued (never pace over pending input) and ends early when the `pace_interrupt` fires
    /// (a release or a submitted turn). A finite sleep that expires with no input just proceeds.
    ///
    /// Lost-wakeup safe: the interrupt future is armed (`enable`) *before* the state is read, so a
    /// release/submit racing the read still wakes the wait (mirrors [`await_settled`]).
    ///
    /// [`await_settled`]: Shared::await_settled
    pub async fn pace_at_seam(&self) {
        let interrupt = self.pace_interrupt.notified();
        tokio::pin!(interrupt);
        interrupt.as_mut().enable(); // register before reading state, so a racing wake isn't lost
        // A submitted turn always wins — deliver it now rather than pacing over it.
        if self.queued.load(Ordering::SeqCst) > 0 {
            return;
        }
        let (speed, held) = self.pace_snapshot();
        if held {
            // Infinite hold: wait until released or a turn is submitted, then proceed.
            interrupt.await;
        } else if !speed.is_zero() {
            // Paced: proceed when the delay elapses or an interrupt (submit/release) ends it early.
            tokio::select! {
                _ = tokio::time::sleep(speed) => {}
                _ = interrupt => {}
            }
        }
        // Not held, no delay → proceed at once.
    }

    /// The loop dequeued a turn: mark running, then consume the queued count — `running` set
    /// *before* the decrement so [`is_settled`] never sees both clear while a turn is in flight.
    ///
    /// [`is_settled`]: Shared::is_settled
    fn dequeued_turn(&self) {
        self.running.store(true, Ordering::SeqCst);
        self.queued.fetch_sub(1, Ordering::SeqCst);
    }

    /// A turn was drained mid-trajectory at the injection juncture
    /// (docs/control-channel.md): consume its `queued` count so `/settle` can
    /// still settle once the trajectory ends, but leave `running` alone — the loop is already
    /// running, so unlike [`dequeued_turn`] there's no busy edge to guard. Mirrors only the
    /// queued-count half of the dequeue.
    ///
    /// [`dequeued_turn`]: Shared::dequeued_turn
    fn injected_turn_dequeued(&self) {
        self.queued.fetch_sub(1, Ordering::SeqCst);
    }

    /// The loop is parking (about to await the next user turn): mark not-running and wake any
    /// `/settle` waiter. If a turn is already queued, `recv` resolves at once and [`dequeued_turn`]
    /// re-sets running, so a transient `running == false` with `queued > 0` still reads as busy.
    ///
    /// [`dequeued_turn`]: Shared::dequeued_turn
    fn parking(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.settle_changed.notify_waiters();
    }

    /// Whether the daemon is at a **decision point** a client can act on: a gate is pending, or the
    /// loop is idle (parked with nothing queued). Mid-turn — running, no gate — is *not* settled.
    /// The predicate behind `/settle` and the `?wait` flag (docs/permissions.md).
    fn is_settled(&self) -> bool {
        self.pending.lock().unwrap().is_some()
            || (!self.running.load(Ordering::SeqCst) && self.queued.load(Ordering::SeqCst) == 0)
    }

    /// Block until [`is_settled`]. Uses `Notified::enable` to register the waiter **before** the
    /// condition check: `settle_changed` is a `notify_waiters` (no stored permit), so a
    /// `parking`/`register_pending` that fires between the check and the await would otherwise be
    /// lost. With the waiter pre-registered, the wake is delivered to the pending future.
    ///
    /// [`is_settled`]: Shared::is_settled
    async fn await_settled(&self) {
        loop {
            let notified = self.settle_changed.notified();
            tokio::pin!(notified);
            // Register on the waiter queue now, so a notify racing the check below isn't missed.
            notified.as_mut().enable();
            if self.is_settled() {
                return;
            }
            notified.await;
        }
    }

    /// The spotter's reason for the most recent verdict on `call_id`, read from the log's `Spot`
    /// audit entries (docs/permissions.md). The spotter logs its verdict *before* routing the call
    /// to the human gate, so by the time the gate registers, the reason is already durably in the
    /// log — no side channel needed. The log is the source of truth, here too.
    fn latest_spot_reason(&self, call_id: &CallId) -> Option<String> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                Payload::Spot(s) if &s.call_id == call_id => Some(s.reason.clone()),
                _ => None,
            })
    }

    /// Register an approval gate and surface it to `GET /state`. Returns the
    /// receiver the loop awaits.
    fn register_pending(&self, call: &ToolCall) -> oneshot::Receiver<Approval> {
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(call.id.clone(), tx);
        let reason = self.latest_spot_reason(&call.id);
        *self.pending.lock().unwrap() = Some(Pending {
            call_id: call.id.0.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            reason,
        });
        // A gate is a settled state — wake any `/settle` waiter (the `?wait` on `/turn` returns here).
        self.settle_changed.notify_waiters();
        rx
    }

    /// Deliver a client's decision to the waiting loop. Clears the gate so
    /// `GET /state` reflects the resolution immediately. `false` → no such gate.
    fn resolve_approval(&self, id: &CallId, decision: Approval) -> bool {
        let sender = self.approvals.lock().unwrap().remove(id);
        let Some(tx) = sender else { return false };
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.as_ref().is_some_and(|p| p.call_id == id.0) {
                *pending = None;
            }
        }
        // The receiver is dropped only if the loop already moved on (e.g. the turn
        // was aborted by another route); treat that as "nothing waiting".
        tx.send(decision).is_ok()
    }

    /// Enqueue an ambient notification (docs/inbox.md — `POST /notify`). If it names a
    /// `slot` (or an explicit `replaces` id) matching a pending entry, that entry is
    /// **superseded**: archived-and-replaced, never edited (records are immutable,
    /// docs/log-and-projection.md). A superseded-while-pending entry is simply dropped — it never reached a
    /// gate, so from the log's perspective it never happened. Returns the new id.
    fn notify(
        &self,
        source: String,
        body: String,
        slot: Option<String>,
        replaces: Option<u64>,
    ) -> u64 {
        let mut inbox = self.inbox.lock().unwrap();
        // Supersede: an explicit `replaces` id, and/or any pending entry on the slot.
        if let Some(old) = replaces {
            inbox.pending.retain(|n| n.id != old);
        }
        if let Some(s) = &slot {
            inbox
                .pending
                .retain(|n| n.slot.as_deref() != Some(s.as_str()));
        }
        let id = inbox.next_id;
        inbox.next_id += 1;
        inbox.pending.push(PendingNotification {
            id,
            source,
            body,
            slot,
        });
        drop(inbox);
        // Wake an idle agent for a standalone triage (docs/inbox.md — wake-on-notification).
        self.notified.notify_waiters();
        id
    }

    /// Whether any **un-shown** notification is queued (the ephemeral pending set).
    /// The idle wake triggers on this — never on already-shown "left" items, so a
    /// deferred notification can't spin the idle loop (docs/inbox.md).
    fn has_pending_notifications(&self) -> bool {
        !self.inbox.lock().unwrap().pending.is_empty()
    }

    /// Resolve when there's an un-shown notification to triage (docs/inbox.md). Lost-wakeup
    /// safe: arm the waiter before the check, so a `notify` landing between the check
    /// and the await still wakes us (the `appended`-tail pattern). The agent loop's
    /// idle `select!` races this against the next user turn.
    pub async fn inbox_wake(&self) {
        loop {
            let waiter = self.notified.notified();
            if self.has_pending_notifications() {
                return;
            }
            waiter.await;
        }
    }

    /// Dismiss a pending notification out-of-band (docs/inbox.md — `POST
    /// /notifications/:id/dismiss`). `false` → no pending notification with that id.
    fn dismiss_notification(&self, id: u64) -> bool {
        let mut inbox = self.inbox.lock().unwrap();
        let before = inbox.pending.len();
        inbox.pending.retain(|n| n.id != id);
        inbox.pending.len() != before
    }

    /// The current pending inbox, in arrival order — surfaced by `GET /state`.
    fn pending_notifications(&self) -> Vec<PendingNotification> {
        self.inbox.lock().unwrap().pending.clone()
    }

    /// Drain the un-shown notifications for a triage gate (docs/inbox.md): take and clear
    /// the ephemeral queue. The loop logs each as a first-show `Ambient` entry (its
    /// `seq` becoming its durable id) and shows it to the model, so the queue holds
    /// only what's arrived since the last gate. Returned in arrival order as
    /// [`Notification`](crate::log::Notification)s ready to wrap in a payload.
    pub fn drain_inbox(&self) -> Vec<crate::log::Notification> {
        let mut inbox = self.inbox.lock().unwrap();
        inbox
            .pending
            .drain(..)
            .map(|p| crate::log::Notification {
                source: p.source,
                body: p.body,
                slot: p.slot,
            })
            .collect()
    }

    /// Spawn a backgrounded bash command into the supervisor (docs/supervisor.md). Returns
    /// its `bg#<n>` handle immediately; a detached task drains the child's output
    /// into the registry (re-surfaced live at each triage gate) and fires an exit
    /// notification when it ends. The child leads a fresh process group, so one
    /// `killpg` reaps it and any grandchildren on teardown.
    pub fn spawn_background(self: &Arc<Self>, command: String) -> io::Result<String> {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // The session scratch dir rides `$TMPDIR` into background children too,
        // mirroring the foreground `Bash` spawn (docs/tools.md).
        if let Some(scratch) = &self.scratch {
            cmd.env("TMPDIR", scratch);
        }
        let mut child = cmd.spawn()?;
        let pgid = child.id();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let handle = self.supervisor.lock().unwrap().register(command, pgid);
        tokio::spawn(drain_background(
            Arc::clone(self),
            handle.clone(),
            child,
            stdout,
            stderr,
        ));
        Ok(handle)
    }

    /// Adopt a *detached foreground* command into the supervisor (the
    /// foreground→background handoff, docs/supervisor.md). Mirrors
    /// [`spawn_background`](Self::spawn_background) from the registration on —
    /// same drain task, same exit notification — but the child is already
    /// running: the capture is seeded with everything the foreground path
    /// drained before the detach, and the drain resumes from that byte boundary.
    pub fn adopt_background(self: &Arc<Self>, detached: crate::bash::DetachedBash) -> String {
        let crate::bash::DetachedBash {
            command,
            child,
            stdout,
            stderr,
            seed,
        } = detached;
        let pgid = child.id();
        let handle = self.supervisor.lock().unwrap().adopt(command, pgid, &seed);
        tokio::spawn(drain_background(
            Arc::clone(self),
            handle.clone(),
            child,
            stdout,
            stderr,
        ));
        handle
    }

    /// `POST /background` (docs/supervisor.md): ask the loop to detach the
    /// running foreground tool into the supervisor, optionally attaching an
    /// operator note the handoff delivers as the next user message. A no-op if
    /// nothing is mid-tool (no waiter → the wake is lost; the note is cleared
    /// when the next foreground bash starts).
    pub fn request_detach(&self, note: Option<String>) {
        *self.detach_note.lock().unwrap() = note.filter(|n| !n.trim().is_empty());
        self.detach.notify_waiters();
    }

    /// Consume the pending detach note — called by the loop when a detach lands.
    pub fn take_detach_note(&self) -> Option<String> {
        self.detach_note.lock().unwrap().take()
    }

    /// The peek's ambient `background` section (docs/supervisor.md) — a snapshot of running,
    /// un-muted processes, taken under the lock so the loop can hand it to the fork.
    pub fn bg_snapshot(&self) -> Vec<BgView> {
        self.supervisor.lock().unwrap().views()
    }

    /// Snapshot a process's current tail for a pin — `(source, body)` ready to wrap
    /// in a [`Notification`](crate::log::Notification) (docs/supervisor.md).
    pub fn bg_pin_snapshot(&self, handle: &str) -> Option<(String, String)> {
        self.supervisor.lock().unwrap().pin_snapshot(handle)
    }

    /// Mute a process's running tail (`dismiss` for a live process, docs/supervisor.md — it keeps
    /// running and still notifies on exit).
    pub fn mute_bg(&self, handle: &str) {
        self.supervisor.lock().unwrap().mute(handle);
    }

    /// The directed `list_processes` view (docs/supervisor.md).
    pub fn bg_list_text(&self) -> String {
        self.supervisor.lock().unwrap().list_text()
    }

    /// Cursor-based read of one process's output (`read_since`, docs/supervisor.md). `None` if the
    /// handle is unknown.
    pub fn bg_read_since(&self, handle: &str, cursor: Option<u64>) -> Option<String> {
        self.supervisor.lock().unwrap().read_since(handle, cursor)
    }

    /// Kill a running background process by handle (docs/supervisor.md — `kill_process`):
    /// `killpg` its group; the drain task then sees EOF and fires the exit
    /// notification. `false` if no such running process. The lock is dropped before
    /// the FFI kill (never held across it).
    pub fn kill_bg(&self, handle: &str) -> bool {
        let pgid = self.supervisor.lock().unwrap().pgid_of(handle);
        match pgid {
            Some(pgid) => {
                crate::bash::kill_group(pgid);
                true
            }
            None => false,
        }
    }

    /// The directed inbox read (docs/inbox.md — `check_inbox`): the active shown
    /// notifications (left at earlier gates) plus the newly-arrived ephemeral ones,
    /// so the model can survey what's waiting on demand. The fork peek for this
    /// tool's own result is where they're actually triaged.
    pub fn inbox_summary(&self) -> String {
        let pending = self.pending_notifications();
        let active = {
            let log = self.log.lock().unwrap();
            crate::inbox::active_shown(log.entries())
        };
        if pending.is_empty() && active.is_empty() {
            return "Inbox empty — no pending notifications.".to_string();
        }
        let mut out = format!(
            "Inbox: {} pending notification(s).",
            pending.len() + active.len()
        );
        for n in &active {
            out.push_str(&format!("\n[{}] {} · {}", n.seq, n.source, n.body));
        }
        for n in &pending {
            out.push_str(&format!("\n(new) {} · {}", n.source, n.body));
        }
        out.push_str("\n(triage them in this step's review.)");
        out
    }

    /// `killpg` every still-running background child — daemon teardown (docs/supervisor.md
    /// ownership). Best-effort: a child that already exited is a benign `ESRCH`.
    pub fn kill_all_bg(&self) {
        for pgid in self.supervisor.lock().unwrap().running_pgids() {
            crate::bash::kill_group(pgid);
        }
    }

    fn interrupt(&self) {
        // Latch first, then wake: a bash tool racing the abort future must never
        // resolve and reach the loop's juncture check before the flag is visible.
        self.interrupt_latch.store(true, Ordering::SeqCst);
        self.interrupt.notify_waiters();
    }

    /// Consume the latched interrupt — `true` exactly once per `POST /interrupt`
    /// (docs/control-channel.md). The loop calls this at both junctures around a
    /// call: before one runs (it doesn't) and where its result lands (the result
    /// is withheld from the wire). Also cleared when a fresh user turn starts, so
    /// an interrupt fired at idle can't stop the *next* turn's first call.
    pub fn take_interrupt(&self) -> bool {
        self.interrupt_latch.swap(false, Ordering::SeqCst)
    }

    fn state_json(&self) -> String {
        let pending = self.pending.lock().unwrap().clone();
        let (pace_speed, pace_held) = self.pace_snapshot();
        let last_seq = self
            .log
            .lock()
            .unwrap()
            .entries()
            .len()
            .checked_sub(1)
            .map(|i| i as u64);
        let metrics = self.metrics.lock().unwrap();
        let usage = serde_json::json!({
            "last": metrics.last_usage,
            "cumulative": {
                "input_tokens": metrics.total_input,
                "output_tokens": metrics.total_output,
                "cache_read_input_tokens": metrics.total_cache_read,
                "cache_creation_input_tokens": metrics.total_cache_creation,
            },
            "seconds_since_last_request": metrics.last_request_at.map(|t| t.elapsed().as_secs()),
        });
        serde_json::json!({
            "last_seq": last_seq,
            "pending_approval": pending,
            // Whether the loop is executing a turn (vs. parked idle) — the "is something happening"
            // signal the TUI's graduated Esc reads to decide pause-vs-swallow
            // (docs/tui.md). Unlike streaming deltas, this stays true
            // through a *silent* long-running tool, so Esc can still reach it.
            "running": self.running.load(Ordering::SeqCst),
            // Autonomous = no synchronous human at the gate (the orchestrator-above wants to know
            // a subordinate runs unsupervised before it trusts the perimeter it reads below).
            // Derived from `review_level`; kept for back-compat.
            "autonomous": self.autonomous(),
            // The review-level dial (docs/permissions.md): who reviews a
            // gated call — `autonomous` / `spotter_human` / `human_only`. Runtime-settable.
            "review_level": self.review_level().as_str(),
            // The operator's pace dial (docs/control-channel.md) — loud to the
            // operator's clients, silent to the agent (never logged/projected). `speed_ms` is the
            // between-turns delay; `held` is the pause overlay.
            "pace": {
                "speed_ms": pace_speed.as_millis() as u64,
                "held": pace_held,
            },
            "role": self.role_json(),
            "pending_notifications": self.pending_notifications(),
            // Async config edits awaiting an orchestrator decision (the prefix-promotion
            // proposal, the session's config-write) — polled by the client's policy surface
            // alongside the gate; resolved over `POST /proposals/:seq`.
            "proposals": self.pending_proposals(),
            // The live background processes (docs/supervisor.md) — the same `views()` snapshot
            // the peek auto-shows the model, surfaced here for the human clients (the
            // TUI process panel, docs/tui.md). Ephemeral, like the rest of `/state`.
            "processes": self.bg_snapshot(),
            "usage": usage,
        })
        .to_string()
    }

    /// The **effective** policy report for `GET /state` — the base config plus every committed
    /// config edit folded in, i.e. the *live* perimeter, not the boot config
    /// (docs/permissions.md). It carries full **contents**, not counts: an
    /// orchestrator inspecting a subordinate (or a human checking "what am I running?") needs the
    /// actual commands/paths/scope in force — and the config is bounded-small by design, never
    /// projected, with no secret to withhold. `scope` and `watch-for` are surfaced here because the
    /// effective scope can drift from the frozen boot preamble (a live scope edit) and `watch-for`
    /// is on no other wire. `source` is the base config's provenance (path, or `null` for the
    /// built-in default).
    fn role_json(&self) -> Value {
        let source = match &self.role.source {
            ConfigSource::Default => Value::Null,
            ConfigSource::Path(p) => Value::String(p.display().to_string()),
        };
        // The live policy, not the frozen base — this is the fix for `/state` reporting the boot
        // config while a session grant / fs-watch edit had already moved the real perimeter.
        let role = self.effective_config();
        let paths: serde_json::Map<String, Value> = role
            .paths
            .iter()
            .map(|(p, perms)| {
                (
                    p.clone(),
                    serde_json::json!({ "read": perms.read, "write": perms.write }),
                )
            })
            .collect();
        serde_json::json!({
            "source": source,
            "scope": role.scope,
            "watch_for": role.watch_for,
            "commands": role.commands,
            "tools": role.tools,
            "paths": paths,
        })
    }

    fn log_json(&self, since: Option<u64>) -> String {
        let log = self.log.lock().unwrap();
        let entries: Vec<&LogEntry> = match since {
            Some(s) => log.entries().iter().filter(|e| e.seq >= s).collect(),
            None => log.entries().iter().collect(),
        };
        serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into())
    }
}

/// The loop's [`Frontend`], backed entirely by [`Shared`]: every input arrives
/// over HTTP. The loop body is unchanged — it still pulls (blocks at
/// `read_user_turn` / `approve`), and the pull resolves when a client pushes.
pub struct RemoteFrontend {
    shared: Arc<Shared>,
    /// Single-consumer receiver of the turn inbox.
    turns: mpsc::UnboundedReceiver<String>,
}

impl Frontend for RemoteFrontend {
    async fn read_user_turn(&mut self) -> Option<String> {
        // Mark the loop parked so `/settle` reports idle while we wait (and wake any waiter).
        self.shared.parking();
        // Blocks until a `POST /turn` arrives. `None` (all senders dropped) only
        // happens once the daemon is tearing down, which ends the loop cleanly.
        let turn = self.turns.recv().await;
        if turn.is_some() {
            // Back to busy: mark running and consume the queued count.
            self.shared.dequeued_turn();
        }
        turn
    }

    fn drain_injected_turns(&mut self) -> Vec<String> {
        // Non-blocking pull of everything queued right now (docs/control-channel.md).
        // Each drained turn consumes its `queued` count — the loop is already running, so we
        // only balance the counter (never touch `running`), or `/settle` would read "busy"
        // forever after the trajectory ends. Blank turns are dropped (still counted): a
        // mid-trajectory empty submit mirrors the idle path's blank-keep-idling, never landing
        // an empty `User` entry on the wire.
        let mut turns = Vec::new();
        while let Ok(text) = self.turns.try_recv() {
            self.shared.injected_turn_dequeued();
            if !text.trim().is_empty() {
                turns.push(text);
            }
        }
        turns
    }

    async fn approve(&mut self, call: &ToolCall) -> Approval {
        let rx = self.shared.register_pending(call);
        // A dropped sender (shouldn't happen on the live daemon) defaults to abort
        // — never run a call we couldn't get a decision for.
        rx.await.unwrap_or(Approval::Abort)
    }

    async fn interrupted(&mut self) -> Interrupt {
        // Race the two operator intents over the running tool: `POST /interrupt`
        // kills, `POST /background` detaches into the supervisor (docs/supervisor.md).
        tokio::select! {
            _ = self.shared.interrupt.notified() => Interrupt::Kill,
            _ = self.shared.detach.notified() => Interrupt::Detach,
        }
    }
}

/// The notification a **session-visible** config edit raises (docs/permissions.md),
/// as `(body, slot)`, or `None` for an edit the session need not hear about. `scope` and `paths`
/// are the perimeter the acting session should learn moved; `commands`/`tools` are classifier-only.
/// The `slot` lets a later edit of the same key supersede a still-pending notice.
fn session_visible_notice(change: &crate::log::ConfigChange) -> Option<(String, String)> {
    use crate::log::ConfigChange;
    match change {
        ConfigChange::Scope { text } => Some((
            format!("Your session scope was updated by the orchestrator:\n\n{text}"),
            "config:scope".to_string(),
        )),
        ConfigChange::Path { path, read, write } => {
            let perms = match (*read, *write) {
                (_, true) => "read + write",
                (true, false) => "read only",
                (false, false) => "no access",
            };
            Some((
                format!(
                    "Filesystem access changed: `{path}` (and everything beneath it) is now {perms}."
                ),
                format!("config:path:{path}"),
            ))
        }
        ConfigChange::Command { .. } | ConfigChange::Tool { .. } => None,
    }
}

/// Cap on a `/settle` (or `?wait`) long-poll: past this, return the current state regardless, so a
/// wedged loop can never hold a client forever. Far longer than any real turn — a clean settle wins
/// the race in practice; this is only the backstop.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Run the daemon: bind the control socket, spawn the HTTP server, and drive the
/// agent loop with a [`RemoteFrontend`]. The loop blocks until a `POST /turn`
/// arrives, so an idle daemon simply waits — retiring argv-seeding and the
/// "backgrounded run can't get its opening prompt" problem (docs/README.md).
pub async fn serve<D: Distiller, S: Spotter>(
    client: Client,
    toolbox: Toolbox,
    distiller: D,
    spotter: S,
    log: Log,
    socket: PathBuf,
    role: LoadedConfig,
    autonomous: bool,
) -> Result<(), Error> {
    let (shared, turns) = Shared::with_role_and_scratch(
        log,
        role,
        toolbox.scratch_dir().map(std::path::Path::to_path_buf),
    );
    // Set before the loop spawns: the gate reads it on the loop's thread (backlog/permissions/
    // session-dir-and-config-as-reviewed-file.md — the autonomous flip).
    shared.set_autonomous(autonomous);
    // Share the client between the loop and the keepalive task. `tools` is computed
    // once here for the ping; `agent::run` recomputes the same (byte-stable, docs/log-and-projection.md)
    // definitions internally, so both send an identical prefix.
    let client = Arc::new(client);
    // The exact tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the supervisor/inbox tools, docs/supervisor.md, docs/inbox.md). Built from the one source
    // (`agent::tool_definitions`) so the keepalive ping warms the *same* cache prefix
    // a real turn reads; any drift here silently breaks idle cache-warming (docs/log-and-projection.md).
    let tools = agent::tool_definitions(&toolbox);
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns,
    };

    // `with_shared_state` carries the Arc into every conn's TypeSet; `listeners()`
    // upgrades to the multi-listener builder (carrying that state over) so we can
    // `bind_uds`. The same builder's `bind_tcp`/`bind_env` is the deferred-TCP
    // path (docs/transport.md) — a config switch, not an abstraction to build.
    let server = trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(router());
    eprintln!("control socket: {}", socket.display());

    // The cache keepalive runs alongside the loop (docs/log-and-projection.md) — a transport-side
    // timer that never touches the log, so it composes with the loop without
    // sharing anything but the read-only client and the telemetry clocks.
    tokio::spawn(keepalive_loop(
        Arc::clone(&client),
        Arc::clone(&shared),
        tools,
    ));

    // The fs-watch initiator (docs/permissions.md): if the config resolved
    // from a file, watch it so external edits take effect live as committed `ConfigEdit`s. The
    // built-in default has no file to watch.
    if let Some(config_path) = shared.config_path().map(|p| p.to_path_buf()) {
        tokio::spawn(config_watch_loop(Arc::clone(&shared), config_path));
    }

    // Trillium owns the signal registry: by default it registers SIGINT/SIGTERM and
    // drives graceful shutdown through its `Swansong`. But the future `serve` awaits
    // is `agent::run`, which parks in `read_user_turn` on the turn channel — whose
    // senders live in the still-alive `Arc<Shared>`, so a C-c that stops the HTTP
    // listener would otherwise leave the loop parked forever (the `kill -9` hang).
    // Hook the *same* Swansong via `interrupt`: it drives the loop normally until
    // shutdown is initiated, then cancels it and resolves to `None`. (`server` is
    // held — not dropped — for the whole await so the listener stays up; it drops on
    // return, after shutdown has already begun.)
    // Keep a handle for teardown — `run` moves the loop's `Arc<Shared>`.
    let teardown = Arc::clone(&shared);
    let run = agent::run(&client, &mut frontend, shared, distiller, spotter, toolbox);
    let outcome = match server.swansong().interrupt(run).await {
        Some(result) => result,
        None => {
            eprintln!("\nshutting down");
            Ok(())
        }
    };
    // Reap any background children before exiting so a killed daemon doesn't orphan
    // a process group (docs/supervisor.md, ownership).
    teardown.kill_all_bg();
    outcome
}

mod background;
mod http;
mod keepalive;
mod sse;
mod watch;

pub(crate) use watch::refresh_and_emit;

use background::drain_background;
use http::router;
use keepalive::keepalive_loop;
use sse::{stream_deltas, tail_events};
use watch::config_watch_loop;

#[cfg(test)]
mod test;
