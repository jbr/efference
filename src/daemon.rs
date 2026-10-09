//! The control channel — the daemon and its HTTP-over-Unix-socket interface
//! (docs/control-channel.md).
//!
//! The HTTP interface is the singular input source: the loop's only [`Frontend`]
//! is [`RemoteFrontend`], fed by the handlers here, so there is no privileged
//! in-process path. Human (TUI), driver, and subagent are all clients of the same
//! socket.
//!
//! The append-only log is the event store the channel needs (docs/README.md).
//! `GET /log?since=<seq>` replays durably; ephemeral control state (a pending
//! approval) lives in [`Session`] and is reported by `GET /state`.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
    Client, Error, RequestUsage, ToolChoice, UpstreamFailureState, UpstreamRetryState, Usage,
    upstream_failure_state, upstream_retry_state,
};

use crate::config::{ConfigSource, LoadedConfig, Refreshed};
use crate::frontend::{Approval, Frontend, Interrupt, SubmittedTurn};
use crate::log::{CallId, EditState, LogEntry, Payload, RawResult, Seq, ToolCall, Turn};
use crate::persist::Log;
use crate::project::{
    ApiMessage, ImagePlacement, Retention, WireTurn, project_semantic, project_with,
};
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
/// and [`ProcessOutput`](Self::ProcessOutput) key on their own ids for the same reason
/// (docs/control-api.md).
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
    /// without them only prose reaches the stream, so a turn spent thinking or emitting a large
    /// tool input publishes nothing and reads as a hung harness (docs/transport.md).
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
    /// spawn-minted id — rendered `p#<n>` in prose, bare here (docs/supervisor.md).
    /// Demuxed by id exactly like
    /// [`ToolOutput`](Self::ToolOutput) is by `call_id`. Lossy: a cold-opened client
    /// seeds its tail from the process resource and layers these on top.
    ProcessOutput { id: u64, chunk: String },
}

/// What a session is, for a client that has to name it on screen: the model it
/// was configured with and the directory it runs in (docs/control-api.md).
///
/// Both are fixed at boot today. They are carried as a state topic rather than a
/// one-shot read so a client learns them from the connect snapshot along with
/// everything else, and so either moving later reaches clients through the
/// mechanism that already delivers whole-topic replacements.
#[derive(Clone, Debug, Serialize)]
pub struct SessionIdentity {
    /// The session id (docs/session-title.md): its directory name under
    /// `.efference/`, the `--name` a client resolves, the address a peer
    /// messages. Fixed at creation.
    pub id: String,
    /// The current title — the last `Title` entry's text (docs/session-title.md),
    /// `None` before the first is written. The one identity field that moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The native model id boot resolved — the same string the system prompt's
    /// identity line states (docs/context.md).
    pub model: String,
    /// The session's working directory, as the daemon knows it: what the tool
    /// layer was built with, rendered as a path string.
    pub cwd: String,
    /// The host connection and native model behind each role, with whether
    /// triage shares the main client. `None` only for a session built without
    /// boot (tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference: Option<crate::inference::InferenceIdentity>,
    /// The session this one handed its work to with `hand_off` (docs/tools.md),
    /// once it has. An attached client follows it there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
}

/// Why a trajectory stopped short of finishing (docs/transport.md).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StallReason {
    /// The stream-abort budget was spent: every attempt died before finalizing.
    RetriesExhausted,
    /// The request itself failed after the transport's own retries.
    RequestFailed,
    /// The upstream named a failure no retry can change — an exhausted plan, a
    /// rejected request, a bad credential — so the loop stalled on the first
    /// attempt rather than spend the schedule (docs/transport.md).
    UpstreamRefused,
    /// The triage gate could not reach its model — the schedule ran out, or the
    /// upstream refused — so the loop holds the batch's results, unshown, until a
    /// retry succeeds or the operator interrupts (docs/redaction-fork.md).
    TriageUnavailable,
    /// The triage model answered every fork and never usably: a harness defect,
    /// logged as one. The loop holds the results the same way
    /// (docs/redaction-fork.md).
    TriageDefect,
}

/// A trajectory that ended without finishing (docs/transport.md).
///
/// The loop returns to idle on both failure paths above, which leaves the
/// daemon's other signals — `running: false`, no gate, an empty inbox once a
/// triage peek has dismissed the notification — identical to a run that
/// finished its work. This topic is the difference a supervising client reads:
/// set when the loop gives up, cleared when a trajectory next starts, so it
/// names the *current* condition rather than a past one (the same clear-on-
/// recovery rule the transport diagnostics follow).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Stall {
    pub reason: StallReason,
    /// How many attempts were spent, where the failure path counts them;
    /// `null` where it does not.
    pub attempts: Option<u32>,
    /// Unix millis at which the loop gave up — absolute, like every other
    /// instant on the control channel, so nothing has to re-emit a ticking
    /// field (docs/control-api.md).
    pub at: u64,
    /// Leads with what happened and what recovers it — the same register the
    /// retry topic's `message` uses.
    pub message: String,
    /// The upstream's own words for the failure that ended the trajectory.
    pub detail: String,
}

/// The config file on disk does not parse **and** could not be restored to the
/// last content that did (`config::Refreshed::RevertFailed`) — the one state the
/// adopted-or-rejected invariant cannot reach (docs/permissions.md).
///
/// It is terminal-until-repaired rather than a notice: the effective policy in
/// memory is still the last good config, but the file no longer shows it, and the
/// file is the perimeter's readable form. Running on indefinitely would mean a
/// session acting under permissions nobody can inspect. Making it terminal
/// introduces no blocked-with-nobody-home state, because a config write is the one
/// operation that is never autonomous — it gates for the orchestrator even under
/// `--autonomous`, so a file in this condition always has an owner.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConfigRepair {
    /// The file a human has to fix.
    pub path: String,
    /// Unix millis at which the daemon parked, absolute like every other instant
    /// on the control channel (docs/control-api.md).
    pub at: u64,
    /// Leads with the condition and what clears it — the same register the
    /// `stall` topic's `message` uses.
    pub message: String,
    /// Why the file did not parse.
    pub parse_error: String,
    /// Why the restore of the last good content also failed.
    pub restore_error: String,
}

/// A session's state and control surface (docs/control-channel.md).
/// The loop and HTTP handlers hold the same `Arc<Session>`; the runtime owns the
/// listener and wires the agent future and auxiliary tasks.
pub struct Session {
    /// Immutable working root for policy and child execution (docs/control-channel.md).
    cwd: PathBuf,
    /// The parent of the session's storage dir — the `.efference/` base every
    /// session in this project shares (docs/tools.md). The in-session `sessions`
    /// mount scopes its listing and target resolution to it. Set once by the
    /// runtime; unset in tests and offline sessions.
    storage_base: std::sync::OnceLock<PathBuf>,
    /// The attachment store (docs/inbox.md), set once by the runtime alongside
    /// its watcher. Unset in tests and offline sessions, where the upload
    /// routes answer that there is nowhere to put a file.
    attachment_store: std::sync::OnceLock<crate::attachments::Store>,
    /// The path the web client addresses this session by (docs/web-bind.md):
    /// `/` on the session's own bind, `/sessions/<id>/` under a host. Set once
    /// by whoever adopts the session — the bind's shape belongs to whoever
    /// binds, never to a session (docs/web-client.md) — and read by the push
    /// sender: a notification opens this path (docs/web-push.md).
    web_path: std::sync::OnceLock<String>,
    /// The provider's cache lifetime, set at boot only when the model has a prompt
    /// cache; the `cold` topic reads it (`daemon/cold.rs`).
    cache_ttl: std::sync::OnceLock<Duration>,
    /// A one-shot consent to read a cold cache (`accept_cold_read`), consumed by the
    /// loop's next cold check (`daemon/cold.rs`).
    cold_consent: AtomicBool,
    /// The loop is parked at its cold check, waiting for consent: a decision point.
    cold_parked: AtomicBool,
    /// Wakes the cold check and the held inbox wake when consent is granted.
    cold_signal: Notify,
    /// The fold in progress or its last failure (`daemon/fold.rs`).
    fold_state: Mutex<Option<fold::FoldState>>,
    /// Wakes the fold task when a fold is asked for.
    fold_signal: Notify,
    /// The loop appends under this lock; handlers snapshot it for `GET /log`.
    /// The lock is only ever held across the
    /// synchronous `project`/append — never an `await`.
    log: Mutex<Log>,
    /// The open requests and running tools, folded from the log as it grows
    /// (`daemon/operations.rs`). Locked after `log`, never before.
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
    /// Who answers this session's gates when the dial reads `upstream`
    /// (docs/permissions.md): the session that commissioned this one, and the host
    /// that can say whether it is still running. Set by `Host::adopt_with`; `None`
    /// for a root session and for a session built without a host. Control state:
    /// never logged, never projected.
    upstream: Mutex<Option<review::Upstream>>,
    /// Fired by `POST /loop/interrupt`; the future `interrupted()` races a running tool,
    /// and the agent client's read loop races it against the stream (docs/control-channel.md).
    interrupt: Arc<Notify>,
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
    /// Pinged on each log append; the `GET /events` tail wakes on it.
    appended: Notify,
    /// The host's attention table, handed over at adopt (`attention.rs`,
    /// docs/web-push.md). Unset for a session outside a host, which treats nobody
    /// as attentive.
    attention: std::sync::OnceLock<Arc<attention::Attention>>,
    /// The push waiting out [`escalation::ESCALATION_DELAY`] because a client was
    /// attending when its moment fired (`escalation.rs`, docs/web-push.md). At most
    /// one; resolution and sightings of the moment empty it.
    escalation: escalation::EscalationSlot,
    /// Whether the loop is **executing a turn** (vs. parked awaiting one). Together with
    /// `queued`, this drives the `/settle` long-poll's "at a decision point?" predicate
    /// ([`Session::is_settled`], docs/permissions.md). The loop owns it:
    /// set true when it dequeues a turn, false when it parks.
    running: AtomicBool,
    /// Turns submitted (`POST /turns`) but not yet dequeued by the loop, in submission order —
    /// the **authority** on what is pending, mirroring what was pushed into `turns_tx`. It closes
    /// the cross-task race in [`Session::is_settled`] (a turn exists even in the instant before the
    /// loop marks itself `running`), it is what `GET /state`'s `queued` topic reads, and it is
    /// what a withdraw removes from (docs/control-channel.md). Because the channel cannot be
    /// searched, the loop **claims** each received turn against this queue and drops one that is
    /// no longer here — that is how a withdrawn turn is never appended. A pending turn is
    /// ephemeral: it is not in the log, so withdrawing one leaves no trace to retract.
    queue: Mutex<std::collections::VecDeque<SubmittedTurn>>,
    /// The `ask` awaiting a reply, if any (docs/supervisor.md). Taken by the next
    /// submitted turn, which it resolves instead of queueing. Lock order: this before
    /// `queue`.
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
    /// projected (nothing is appended for it at all).
    continue_pending: AtomicBool,
    /// Wakes the idle loop when [`continue_pending`](Self::continue_pending) is set.
    /// Paired with the flag rather than standing alone: `notify_waiters` stores no
    /// permit, so the flag is what a wake landing before the loop parks lands on.
    continue_signal: Notify,
    /// How many sessions this one is subscribed to (docs/tools.md) — host control
    /// state mirrored here so the `wait` tool can count a subscribed session as
    /// something that may wake it (docs/supervisor.md).
    subscriptions: std::sync::atomic::AtomicUsize,
    /// Pinged when a notification is enqueued (docs/inbox.md — `notify`). The agent loop's
    /// idle path awaits this to wake an idle agent for a standalone triage (the
    /// wake-on-notification model that replaced blocking `wait`).
    notified: Notify,
    /// Fan-out for ephemeral pre-commit output deltas (docs/README.md). Producers (the
    /// loop's `complete` sink and the bash tool) publish via [`Session::publish_delta`];
    /// each control-stream connection wanting `output` frames subscribes. Lossy by
    /// design — see [`StreamDelta`].
    stream_tx: broadcast::Sender<StreamDelta>,
    /// Pinged when a state topic may have moved (docs/control-api.md). One notify for
    /// every topic, not one each: the reader recomputes everything and diffs, so a missed
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
    /// The retry in flight, shared with the HTTP client and with both retry loops
    /// (the turn loop and the fork peek), and surfaced by `GET /state` as its own
    /// level-triggered topic. Unlike the failure cell above it carries its own
    /// change signal — a retry pause is *not* followed by an append, so
    /// `watch_topics`'s wake-on-`appended` fallback would leave the topic late for
    /// the whole pause (docs/transport.md). Transport-only: never logged or
    /// projected.
    upstream_retry: UpstreamRetryState,
    /// The Codex backend's subscription windows as of the last response that
    /// carried them (`api/plan.rs`), shared with the HTTP client and published
    /// under `usage.plan`. `None` for every other provider. Transport-only.
    plan: crate::api::PlanState,
    /// Cumulative triage retention (docs/redaction-fork.md), folded lazily from the
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
    capabilities: Mutex<Option<crate::api::ModelCapabilities>>,
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
    /// on a failed revert, cleared by the same watcher the moment a refresh
    /// parses — the human fixes the file, fs-watch picks it up, the gate clears.
    config_repair: Mutex<Option<ConfigRepair>>,
    /// Fires when [`Self::config_repair`] clears, so the parked loop wakes without
    /// polling. Same lost-wakeup discipline as the pace hold: arm before reading.
    config_repaired: Notify,

    /// The ephemeral inbox of pending ambient notifications (docs/inbox.md): out-of-band
    /// events awaiting their first triage gate. Control state, *not* a log payload —
    /// a notification only becomes a durable `Ambient` entry when a fork peek first
    /// shows it. Superseded/dismissed-while-pending entries are dropped here
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
    /// Fired each time a background child is reaped (`drain_background` after
    /// `child.wait()`), so teardown can await the children actually being gone
    /// rather than the kill signal merely having been sent
    /// (docs/supervisor.md).
    reaped: tokio::sync::Notify,
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
    /// [`crate::policy::probe_ledger`], maintained incrementally as the loop assesses moves), never on
    /// the wire — like `effective`, behind a lock because the loop mutates it mid-session.
    probes: Mutex<crate::policy::ProbeLedger>,
    /// The session's resolved `[env]` policy (docs/env.md): the strip set every
    /// background spawn applies, and the masker installed on the log. Not behind
    /// a lock — the strip list is fixed at boot, and the masker owns its own
    /// append-only interior state, so a config edit adds needles without
    /// replacing the policy.
    env_policy: crate::env_table::EnvPolicy,
    /// The operator's **review level** (docs/permissions.md), stored as an
    /// `AtomicU8` ordinal so it is lock-free and **runtime-mutable** (the `PATCH /loop {review_level}` verb,
    /// not just a boot flag). The loop's cascade reads it ([`Session::review_level`]) to decide who
    /// reviews a gated call: `Autonomous` redirects the model instead of blocking on an absent
    /// human; `Upstream` holds the call and routes it to the creating session
    /// ([`Session::resolved_review_level`] is what decides, since an upstream that has ended
    /// reviews as `SpotterHuman`); `SpotterHuman` runs the spotter then the human gate;
    /// `HumanOnly` skips the spotter entirely and gates straight to the human. Every position
    /// but `Autonomous` holds the call — they differ in who is asked. Orthogonal to pace. (The **config-write** gate is
    /// never flipped by this — a session's edit to its own config is always held for the
    /// orchestrator-above.)
    review_level: AtomicU8,
    /// The operator's **pace** dial (docs/control-channel.md): a between-turns
    /// wall-clock control that is *transparent to the agent*. Pure operator-view state — never a
    /// log entry, never projected — so a held or slowed loop is byte-identical on the wire to a
    /// fast one (that, plus timestamps-never-projected, is the invisibility guarantee). Read at the
    /// seam by [`Session::pace_at_seam`].
    pace: Mutex<Pace>,
    /// Wakes an in-flight [`pace_at_seam`](Session::pace_at_seam) sleep so a release or a submitted
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
    /// ([`crate::expand`]): `{TMPDIR: scratch}` when a scratch dir exists, else
    /// empty — every `$NAME` then errors, which is the fail-closed default.
    expander: crate::expand::Expander,
    /// The session's full tool catalog, keyed by wire name — every name the
    /// turn's `tools[]` can carry, the toolbox's mounted providers *and* the
    /// harness's own loop-owned family (`consolidate`, the supervisor/inbox
    /// tools, the two peek tools). Snapshotted by [`Session::set_tool_catalog`]
    /// (`agent::tool_catalog`) before the toolbox moves into the loop, and
    /// read back by `GET /v1/tools/:name` (docs/control-api.md). The mounted
    /// set is fixed for the session (docs/log-and-projection.md), so this is
    /// written once and never reconciled. Empty in tests and offline verbs
    /// that never built a toolbox — the route answers `404` for every name
    /// there, which is the correct "no daemon behind this session" fallback
    /// rather than a distinct error state.
    tools: Mutex<HashMap<String, crate::tools::ToolEntry>>,
}

/// What a [`Session`] is built from: the working directory and log it owns, the
/// role it starts under, and the shared cells its client already holds.
///
/// Named fields rather than a parameter list, because four of these are opaque
/// shared-state handles and a positional mistake among them would compile.
/// [`Self::new`] fills every field a caller without a provider client has no
/// value for, so adding a dependency to a session touches the sites that supply
/// it rather than every site that constructs one.
pub(crate) struct SessionParts {
    pub cwd: PathBuf,
    pub log: Log,
    pub role: LoadedConfig,
    pub scratch: Option<PathBuf>,
    pub upstream_failure: UpstreamFailureState,
    pub upstream_retry: UpstreamRetryState,
    pub plan: crate::api::PlanState,
    /// Chosen from the provider at boot and fixed thereafter, so the canonical
    /// wire and the fork's cached prefix project byte-identically
    /// (docs/log-and-projection.md).
    pub image_placement: ImagePlacement,
    pub env_policy: crate::env_table::EnvPolicy,
}

impl SessionParts {
    /// The session's own two pieces, with every shared cell freshly allocated —
    /// the shape a caller with no provider client behind it wants. A caller that
    /// has one overrides the cells it holds.
    pub(crate) fn new(cwd: PathBuf, log: Log) -> Self {
        Self {
            cwd,
            log,
            role: LoadedConfig::default(),
            scratch: None,
            upstream_failure: upstream_failure_state(),
            upstream_retry: upstream_retry_state(),
            plan: crate::api::plan_state(),
            image_placement: ImagePlacement::default(),
            env_policy: crate::env_table::EnvPolicy::default(),
        }
    }
}

impl Session {
    /// The root used by policy and child processes (docs/control-channel.md).
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }
    /// Name the session's web path (docs/web-bind.md): `/sessions/<id>/` under
    /// a host, `/` on the session's own bind. Called once, by the host's
    /// adopt; a session daemon never calls it and keeps the root default.
    pub fn set_web_path(&self, path: String) {
        let _ = self.web_path.set(path);
    }

    /// The path a push notification opens for this session (docs/web-push.md):
    /// the web path the client addresses it by, `/` before a host has named it.
    pub fn web_path(&self) -> &str {
        self.web_path.get().map(String::as_str).unwrap_or("/")
    }

    /// Record the session's storage base — the parent of `SessionPaths::storage`,
    /// the `.efference/` every session in this project shares (docs/tools.md).
    /// Called once by the runtime; unset in tests and offline sessions.
    pub fn set_storage_base(&self, storage: &std::path::Path) {
        let base = storage
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| storage.to_path_buf());
        let _ = self.storage_base.set(base);
    }

    /// The storage base this session shares with its project's other sessions,
    /// when the runtime set one (docs/tools.md).
    pub fn storage_base(&self) -> Option<&std::path::Path> {
        self.storage_base.get().map(PathBuf::as_path)
    }

    /// The environment every child this session spawns receives — the base its
    /// `bash` children get, strips and injections applied (docs/env.md). Handed
    /// to a session-driven `create` as the new session's base.
    pub fn child_env(&self) -> crate::env_table::ChildEnv {
        self.env_policy.child_env()
    }
    /// The transcript window the in-session `transcript` tool serves
    /// (docs/tools.md): the same selection as `GET /transcript`, with the
    /// daemon's own argument check (docs/control-api.md). The `LogSelect` type
    /// stays private to the daemon, so this is the one public shape.
    pub(crate) fn transcript_window(
        &self,
        since: Option<u64>,
        tail: Option<u64>,
    ) -> std::result::Result<String, String> {
        if since.is_some() && tail.is_some() {
            return Err("since and tail are mutually exclusive".into());
        }
        Ok(self.transcript(&LogSelect {
            since,
            before: None,
            tail,
        }))
    }

    /// Build state under the default role at an absolute working root
    /// (docs/control-channel.md). Relative roots are a caller error and panic.
    pub fn new(cwd: PathBuf, log: Log) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role(cwd, log, LoadedConfig::default())
    }

    /// Build the shared state under a resolved role and hand back the turn-inbox
    /// receiver for the loop.
    pub fn with_role(
        cwd: PathBuf,
        log: Log,
        role: LoadedConfig,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role_and_scratch(cwd, log, role, None)
    }

    /// The full constructor: [`with_role`](Self::with_role) plus the session
    /// scratch dir (docs/tools.md), whose value seeds the structured-tool path
    /// expander and the background spawn env.
    pub fn with_role_and_scratch(
        cwd: PathBuf,
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        Self::with_role_scratch_and_upstream_failure(
            cwd,
            log,
            role,
            scratch,
            upstream_failure_state(),
        )
    }

    /// The daemon constructor with the upstream client's shared transport
    /// diagnostic (docs/transport.md). Tests normally use the public convenience
    /// constructors above, which allocate an empty cell.
    pub(crate) fn with_role_scratch_and_upstream_failure(
        cwd: PathBuf,
        log: Log,
        role: LoadedConfig,
        scratch: Option<PathBuf>,
        upstream_failure: UpstreamFailureState,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        // Tests and the convenience constructors resolve the `[env]` policy from
        // the role they were handed (docs/env.md); boot builds one earlier —
        // it needs the strip set for the toolbox and the masker for the boot
        // preamble — and passes it to [`Session::full`] so the session has exactly
        // one, whose needle set is append-only for its lifetime.
        let base = crate::env_table::EnvSnapshot::capture().expect("valid local environment");
        let resolver = Arc::new(crate::env_table::Resolver::with_environment(
            cwd.clone(),
            base.clone(),
        ));
        let env_policy =
            crate::env_table::EnvPolicy::build_snapshot(&role.role.env, base, &[], resolver).policy;
        Self::full(SessionParts {
            role,
            scratch,
            upstream_failure,
            env_policy,
            ..SessionParts::new(cwd, log)
        })
    }

    /// The full constructor.
    pub(crate) fn full(parts: SessionParts) -> (Arc<Self>, mpsc::UnboundedReceiver<SubmittedTurn>) {
        let SessionParts {
            cwd,
            mut log,
            mut role,
            scratch,
            upstream_failure,
            upstream_retry,
            plan,
            image_placement,
            env_policy,
        } = parts;
        assert!(cwd.is_absolute(), "session cwd must be absolute");
        if let ConfigSource::Path(path) = &mut role.source {
            *path = cwd.join(&*path);
        }
        // Install the mask before the log takes another append (docs/env.md).
        // Masking is irreversible filtering upstream of the log, so it has to be
        // in force at the choke point rather than applied to a stored entry.
        log.set_masker(env_policy.masker());
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
        let probes = crate::policy::probe_ledger(&cwd, &effective, log.entries());
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
            cwd,
            storage_base: std::sync::OnceLock::new(),
            attachment_store: std::sync::OnceLock::new(),
            web_path: std::sync::OnceLock::new(),
            cache_ttl: std::sync::OnceLock::new(),
            cold_consent: AtomicBool::new(false),
            cold_parked: AtomicBool::new(false),
            cold_signal: Notify::new(),
            fold_state: Mutex::new(None),
            fold_signal: Notify::new(),
            operations: Mutex::new(operations::Operations::fold(log.entries())),
            log: Mutex::new(log),
            image_placement,
            turns_tx,
            approvals: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            upstream: Mutex::new(None),
            interrupt: Arc::new(Notify::new()),
            interrupt_latch: AtomicBool::new(false),
            detach: Notify::new(),
            detach_note: Mutex::new(None),
            appended: Notify::new(),
            attention: std::sync::OnceLock::new(),
            escalation: Arc::default(),
            running: AtomicBool::new(false),
            queue: Mutex::new(std::collections::VecDeque::new()),
            ask: Mutex::new(None),
            settle_changed: Notify::new(),
            continue_pending: AtomicBool::new(false),
            continue_signal: Notify::new(),
            subscriptions: std::sync::atomic::AtomicUsize::new(0),
            notified: Notify::new(),
            stream_tx,
            state_changed: Notify::new(),
            state_tx,
            metrics: Mutex::new(Metrics::default()),
            usage_log: Mutex::new(None),
            upstream_failure,
            upstream_retry,
            plan,
            retention: Mutex::new((0, Retention::default())),
            capabilities: Mutex::new(None),
            cost: Arc::new(Mutex::new(CostCell::default())),
            identity: Mutex::new(None),
            stall: Mutex::new(None),
            config_repair: Mutex::new(None),
            config_repaired: Notify::new(),
            inbox: Mutex::new(Inbox::default()),

            proposals: Mutex::new(Vec::new()),
            supervisor: Mutex::new(Supervisor::with_minted(minted)),
            reaped: tokio::sync::Notify::new(),
            role,
            effective: Mutex::new(effective),
            config_file: Mutex::new(config_file),
            probes: Mutex::new(probes),
            env_policy,
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
            tools: Mutex::new(HashMap::new()),
        });
        (shared, turns_rx)
    }

    /// Snapshot the full session tool catalog for `GET /v1/tools/:name`
    /// (docs/control-api.md) — `agent::tool_catalog`, which mirrors
    /// `agent::tool_definitions`: the toolbox's mounted tools *and* the
    /// harness's own loop-owned family (`consolidate`, the supervisor/inbox
    /// tools, the two peek tools), the same union `tools[]` advertises.
    /// Called once by the runtime, before `toolbox` moves into the loop — the
    /// same "only a served session has this" reason as
    /// [`Self::set_capabilities`]/[`Self::set_identity`], which it sits beside.
    pub fn set_tool_catalog(&self, toolbox: &crate::tools::Toolbox) {
        *self.tools.lock().unwrap() = crate::agent::tool_catalog(toolbox);
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
    pub fn set_capabilities(&self, capabilities: crate::api::ModelCapabilities) {
        *self.capabilities.lock().unwrap() = Some(capabilities);
    }

    /// Store what this session is — the model it was configured with and the
    /// directory it runs in (docs/control-api.md). Called once by the runtime before
    /// the loop starts; the `session_identity` topic reads it back.
    pub fn set_identity(&self, identity: SessionIdentity) {
        *self.identity.lock().unwrap() = Some(identity);
        self.touch_state();
    }

    /// What this session is, as the `session_identity` topic reports it; `None`
    /// where no session booted.
    pub fn identity(&self) -> Option<SessionIdentity> {
        self.identity.lock().unwrap().clone()
    }

    /// Record a title decision (docs/session-title.md): append the `Title` entry
    /// and move the identity's `title`, so the topic republishes. `Ok(None)` when
    /// no identity was set (a session read with no daemon behind it), in which
    /// case nothing is appended either.
    pub fn set_title(&self, title: crate::log::Title) -> io::Result<Option<Seq>> {
        let mut guard = self.identity.lock().unwrap();
        let Some(identity) = guard.as_mut() else {
            return Ok(None);
        };
        let seq = self.append(self.latest_turn(), Payload::Title(title.clone()))?;
        identity.title = Some(title.text);
        drop(guard);
        self.touch_state();
        Ok(Some(seq))
    }

    /// Record the session this one handed off to (docs/tools.md), so the
    /// `session_identity` topic republishes and attached clients follow it.
    pub fn set_successor(&self, successor: &str) {
        if let Some(identity) = self.identity.lock().unwrap().as_mut() {
            identity.successor = Some(successor.to_string());
        }
        self.touch_state();
    }

    /// Record that the trajectory gave up (docs/transport.md) — the loop is
    /// returning to idle without finishing. Read back by the `stall` topic.
    pub(crate) fn set_stall(&self, stall: Stall) {
        *self.stall.lock().unwrap() = Some(stall);
        // A stall is a decision point (`is_settled`): a held gate reaches it with the
        // loop still running, so the settle waiters are woken here, not only at park.
        self.settle_changed.notify_waiters();
        self.touch_state();
    }

    /// Clear the stall: a trajectory is starting, or a held gate is retrying, so the
    /// last outcome is no longer the current condition. Called when the work
    /// resumes rather than on a successful completion; the one time the topic is set
    /// while the loop runs is a held triage gate (docs/redaction-fork.md).
    pub(crate) fn clear_stall(&self) {
        let mut stall = self.stall.lock().unwrap();
        if stall.is_none() {
            return;
        }
        *stall = None;
        drop(stall);
        self.touch_state();
    }

    /// The trajectory-stalled condition, or `None` when the last trajectory
    /// finished (or none has run).
    pub(crate) fn stall(&self) -> Option<Stall> {
        self.stall.lock().unwrap().clone()
    }

    /// Park the loop on "config unparseable — human must repair"
    /// (docs/permissions.md). Idempotent: repeated failed saves replace the
    /// condition rather than stacking, exactly as the revert notice supersedes on
    /// its stable slot.
    pub(crate) fn require_config_repair(&self, repair: ConfigRepair) {
        *self.config_repair.lock().unwrap() = Some(repair);
        self.touch_state();
    }

    /// The file parses again: clear the park and wake whoever is waiting on it.
    /// Called from the same refresh that sets it, on every outcome that leaves
    /// valid content on disk — an adopted edit, or a revert that succeeded.
    pub(crate) fn clear_config_repair(&self) {
        let mut repair = self.config_repair.lock().unwrap();
        if repair.is_none() {
            return;
        }
        *repair = None;
        drop(repair);
        self.config_repaired.notify_waiters();
        self.touch_state();
    }

    /// The config-repair condition, or `None` when the file parses.
    pub(crate) fn config_repair(&self) -> Option<ConfigRepair> {
        self.config_repair.lock().unwrap().clone()
    }

    /// Wait until the config file parses again (docs/permissions.md). Returns
    /// immediately when there is nothing to repair, which is the normal case.
    ///
    /// Lost-wakeup safe by the same discipline as the pace hold: the wake future is
    /// armed before the state is read, so a repair landing in between still wakes
    /// the wait. Re-checks after each wake, so a spurious notify cannot release a
    /// park that is still warranted.
    pub async fn await_config_repair(&self) {
        loop {
            let repaired = self.config_repaired.notified();
            tokio::pin!(repaired);
            repaired.as_mut().enable(); // arm before reading, so a racing repair isn't lost
            let Some(repair) = self.config_repair() else {
                return;
            };
            eprintln!("[config: parked — {}]", repair.message);
            repaired.await;
        }
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
            retention.fold_all(fresh, entries);
            *cursor = entries.len();
        }
        *retention
    }

    /// Arm session cost telemetry (docs/transport.md): every recorded request
    /// spawns a fetch of its billed cost from this endpoint's accounting
    /// surface. Called by the runtime only when the provider has one
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
        source: Option<String>,
    ) -> io::Result<Seq> {
        // User turns only — a submitted turn is never a `ConfigEdit`, so none of
        // `append`'s proposal-lifecycle handling applies here.
        let seq = {
            let mut log = self.log.lock().unwrap();
            let seq = log.append_submission(turn, payload, Some(submission_id), source)?;
            self.observe_operations(&log);
            seq
        };
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
        let seq = {
            let mut log = self.log.lock().unwrap();
            let seq = log.append(turn, payload)?;
            self.observe_operations(&log);
            seq
        };
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
        project_with(self.log.lock().unwrap().entries(), self.image_placement)
    }

    /// The dialect-neutral fold of the current log (docs/log-and-projection.md) —
    /// what a request encodes. Lock held only for the synchronous fold.
    pub fn project_turns(&self) -> Vec<WireTurn> {
        project_semantic(self.log.lock().unwrap().entries())
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
    pub(crate) fn latest_turn(&self) -> Turn {
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
    /// time, so it never clones the whole log. The loop reads back the result it
    /// just captured, to hand triage the bytes the log holds (docs/redaction-fork.md).
    pub(crate) fn entry_at(&self, seq: u64) -> Option<LogEntry> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .get(seq as usize)
            .cloned()
    }

    /// The calls that ran with the operator's consent, folded under the log lock so
    /// the whole log is never cloned (docs/permissions.md). The decision rung's
    /// precedent evidence.
    pub(crate) fn consented_calls(&self) -> Vec<crate::log::ToolCall> {
        crate::spotter::precedent::consented(self.log.lock().unwrap().entries())
    }
}

/// Cap on a `/settle` (or `?wait`) long-poll: past this, return the current state regardless, so a
/// wedged loop can never hold a client forever. Far longer than any real turn — a clean settle wins
/// the race in practice; this is only the backstop.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The directory every session dir lives under, relative to the working directory a
/// session was started in (docs/control-channel.md). One session is
/// `<SESSION_BASE>/<id>/`, holding standard-named files.
pub const SESSION_BASE: &str = ".efference";

/// The control socket's name inside a session dir. The layout is named here rather
/// than in each reader because two of them share it: the host and the CLI's socket
/// resolution.
pub const SOCKET_NAME: &str = "control.sock";

mod approvals;
mod ask;
mod attachments;
mod background;
mod cold;
mod config_edits;
mod continuation;
mod entry_view;
mod fold;
// The typed error envelope (docs/control-api.md) — every non-2xx body the router
// answers with.
pub(crate) mod errors;
// The file viewer's read surface (docs/web-bind.md), mounted on the TCP spawn only.
pub(crate) mod files;
// The control-stream frame envelope (docs/control-api.md). `dead_code` covers
// `Frame::with_render` and `Frame::state`, which the emitter does not reach:
// nothing annotates a frame with a render yet, and the state path builds from
// pre-serialized topic JSON (`Frame::state_raw`).
#[allow(dead_code)]
mod frames;
mod handler;
mod http;
mod keepalive;
mod metrics;
mod notifications;
mod operations;
mod pace;
mod status;
mod subscriptions;
// Whether any client is attending the host (docs/web-push.md), and how a moment's
// push waits on it.
pub(crate) mod attention;
mod escalation;
mod push;
mod stream;
pub(crate) mod tcp;
mod topics;
mod transcript;
mod views;
mod watch;

/// The one handler construction (docs/web-bind.md): every listener mounts
/// [`handler`], keyed on its [`Transport`] and [`App`]. Public so a client of the
/// control channel can mount the socket-shaped stack on an in-memory transport and
/// exercise a real request/response cycle without binding anything.
pub use handler::{App, Transport, handler};

pub(crate) use http::cold_gate;
pub(crate) use watch::refresh_and_emit;

use approvals::Pending;

pub use crate::log::RequestOrigin;
use config_edits::{PendingProposal, session_visible_notice};
use entry_view::{EntryView, result_raw};
use errors::{ErrorBody, ErrorCode, fail, respond};
use frames::{Frame, Topic};
pub use metrics::{COLD_STREAK_WARN, CacheRead, ColdStart, ColdStreak, UsageLine};
use metrics::{CostCell, Metrics};
use notifications::Inbox;
use pace::Pace;
use stream::{Include, control_stream};
use topics::{StateUpdate, watch_topics};

use attachments::attachments_watch_loop;
use http::LogSelect;
pub use http::mint_submission_id;
use keepalive::keepalive_loop;
pub use keepalive::{believed_cold_since, keepalive_interval};
use watch::config_watch_loop;

#[cfg(test)]
mod test;

mod frontend;
mod host;
pub mod island;
mod paths;
mod review;
mod runtime;
pub mod slug;
pub use host::{
    ArchivedSession, CreateSession, Host, PatchSession, SessionOptionsRequest, SessionState,
    SessionSummary, serve_host,
};
pub use paths::SessionPaths;
pub use runtime::{SessionBoot, SessionRuntime};

pub use frontend::RemoteFrontend;
pub use tcp::{TcpBind, TlsPem};
