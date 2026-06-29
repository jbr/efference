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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use crate::config::{ConfigSource, LoadedConfig};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend};
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
/// the stream is interface candy with the durable log as its backstop — which is
/// exactly why it can ride a lossy `broadcast` where `/events` must tail the log.
#[derive(Clone)]
pub enum StreamDelta {
    /// A fragment of assistant prose as the model streams it from the upstream SSE.
    AssistantText(String),
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
/// once enqueued — supersession archives-and-replaces rather than editing (docs/substrate.md).
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
/// (backlog/permissions/proposal-lifecycle.md). Filed by a `Proposed` [`ConfigEdit`] append (the
/// session's `propose_scope`, the spotter's prefix-promotion); the orchestrator-above commits or
/// cancels it, which drops it from the queue. Unlike [`PendingNotification`], the proposal **already
/// has a durable `seq`** (the `Proposed` entry's), so that *is* its id — no queue-local counter.
#[derive(Clone, Debug, Serialize)]
struct PendingProposal {
    /// The `seq` of the `Proposed` log entry — the proposal's id, which the orchestrator's
    /// commit/cancel references.
    seq: Seq,
    /// The change being proposed; the committing entry copies it self-contained so the fold stays a
    /// pure per-entry filter (backlog/permissions/proposal-lifecycle.md).
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
    /// Pinged on each log append; the SSE tail (next slice) will wake on it.
    appended: Notify,
    /// Whether the loop is **executing a turn** (vs. parked awaiting one). Together with
    /// `queued`, this drives the `/settle` long-poll's "at a decision point?" predicate
    /// ([`Shared::is_settled`], backlog/permissions/proposal-lifecycle.md). The loop owns it:
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
    /// predict TTL eviction (docs/substrate.md). Transport-only: never touches the log.
    metrics: Mutex<Metrics>,
    /// The ephemeral inbox of pending ambient notifications (docs/inbox.md): out-of-band
    /// events awaiting their first triage gate. Control state, *not* a log payload —
    /// a notification only becomes a durable `Ambient` entry when a fork peek first
    /// shows it (Slice C). Superseded/dismissed-while-pending entries are dropped here
    /// (the "never-happened" case); only shown ones reach the audit log.
    inbox: Mutex<Inbox>,
    /// The pending-proposals queue: async `ConfigEdit`s (the session's `propose_scope`, the
    /// spotter's prefix-promotion) awaiting an orchestrator's commit/cancel
    /// (backlog/permissions/proposal-lifecycle.md). Ephemeral control state like `inbox` — a
    /// `Proposed` entry is durable in the log, but its *pending* status lives here until resolved.
    proposals: Mutex<Vec<PendingProposal>>,
    /// The background-process registry (docs/supervisor.md): live children whose running tail
    /// is re-sampled fresh each triage gate. Ephemeral control state like `inbox` /
    /// `metrics` — never logged, never projected (a running tail forced through the
    /// immutable notification model would pile up stale snapshots). Pinned tails and
    /// exits *do* reach the durable log, but as `Ambient` notifications, not from here.
    supervisor: Mutex<Supervisor>,
    /// The resolved role config and where it came from (backlog/permissions/README.md).
    /// Read-only for the session's lifetime — the scope is already frozen into the
    /// cached preamble; this copy backs the `GET /state` report ("which role am I?") and
    /// is where the cascade rungs (the prefix fast-path and the spotter) read the
    /// allowlist, path grants, scope, and watch-for from. Immutable, so no lock.
    role: LoadedConfig,
    /// The **effective** policy = the base `role` + the fold of every `ConfigEdit` appended so
    /// far (backlog/permissions/config-edit.md). Maintained incrementally in [`append`]
    /// as grants land, so a gate reads current policy in O(1) without re-scanning the log.
    /// Behind a lock because grants mutate it mid-session (unlike the frozen base `role`);
    /// the gate takes an `Arc` clone, cheap and safe to hold across the spotter's await.
    ///
    /// [`append`]: Shared::append
    effective: Mutex<Arc<crate::config::Config>>,
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
    /// keepalive cap (docs/substrate.md).
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
        let shared = Arc::new(Self {
            log: Mutex::new(log),
            turns_tx,
            approvals: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            interrupt: Notify::new(),
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
        });
        (shared, turns_rx)
    }

    /// Record a completed request's `usage` and stamp the wall clock. The loop
    /// calls this once per API round-trip; `GET /state` reports the latest plus
    /// running totals and the seconds-since (the eviction-prediction signal,
    /// docs/substrate.md). Transport-only — no log append, no projection change.
    pub fn record_request(&self, usage: Usage) {
        let mut m = self.metrics.lock().unwrap();
        Self::fold_usage(&mut m, usage);
        m.last_real_turn_at = Some(Instant::now());
    }

    /// A keepalive ping's usage (docs/substrate.md). Folded into the telemetry and the
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
        // A `ConfigEdit` drives the proposal lifecycle (backlog/permissions/proposal-lifecycle.md):
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
                    // re-renders the cached preamble (backlog/permissions/config-edit.md). It rides
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
    /// (backlog/permissions/proposal-lifecycle.md). Surfaced to the orchestrator-above by the HTTP
    /// layer (the API shape is the next ring out); for now this backs tests and the commit/cancel
    /// verbs. `allow(dead_code)`: the reader seam is wired ahead of the HTTP surface that consumes it.
    #[allow(dead_code)]
    fn pending_proposals(&self) -> Vec<PendingProposal> {
        self.proposals.lock().unwrap().clone()
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
    /// (backlog/permissions/prefix-fastpath.md). Immutable for the session, so a borrow
    /// is fine — no lock.
    pub fn role(&self) -> &crate::config::Config {
        &self.role.role
    }

    /// The effective policy the cascade should consult: base role + the fold of every grant
    /// appended so far (backlog/permissions/config-edit.md). Returns an `Arc` clone of the
    /// incrementally-maintained role — O(1), and safe to hold across the spotter's await (a
    /// lock guard would not be). Falls back to the base if no grants have landed.
    pub fn effective_config(&self) -> Arc<crate::config::Config> {
        self.effective.lock().unwrap().clone()
    }

    /// The turn of the most recent log entry (`0` if empty) — the turn an **out-of-band** append
    /// attaches to (the fs-watch initiator, which has no loop-turn context,
    /// backlog/permissions/proposal-lifecycle.md). A `ConfigEdit` projects to nothing, so its turn
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
    /// (backlog/permissions/config-edit.md).
    pub fn config_path(&self) -> Option<&std::path::Path> {
        match &self.role.source {
            ConfigSource::Path(p) => Some(p),
            ConfigSource::Default => None,
        }
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
        // backlog/permissions/proposal-lifecycle.md).
        self.queued.fetch_add(1, Ordering::SeqCst);
        // The receiver lives in the loop's `RemoteFrontend`; a send fails only if
        // the loop has exited, in which case the daemon is shutting down anyway.
        let _ = self.turns_tx.send(text);
    }

    /// The loop dequeued a turn: mark running, then consume the queued count — `running` set
    /// *before* the decrement so [`is_settled`] never sees both clear while a turn is in flight.
    ///
    /// [`is_settled`]: Shared::is_settled
    fn dequeued_turn(&self) {
        self.running.store(true, Ordering::SeqCst);
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
    /// The predicate behind `/settle` and the `?wait` flag (backlog/permissions/proposal-lifecycle.md).
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

    /// Register an approval gate and surface it to `GET /state`. Returns the
    /// receiver the loop awaits.
    fn register_pending(&self, call: &ToolCall) -> oneshot::Receiver<Approval> {
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(call.id.clone(), tx);
        *self.pending.lock().unwrap() = Some(Pending {
            call_id: call.id.0.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
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
    /// docs/substrate.md). A superseded-while-pending entry is simply dropped — it never reached a
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
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
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
        self.interrupt.notify_waiters();
    }

    fn state_json(&self) -> String {
        let pending = self.pending.lock().unwrap().clone();
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
            "role": self.role_json(),
            "pending_notifications": self.pending_notifications(),
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
    /// (backlog/permissions/proposal-lifecycle.md). It carries full **contents**, not counts: an
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

    async fn approve(&mut self, call: &ToolCall) -> Approval {
        let rx = self.shared.register_pending(call);
        // A dropped sender (shouldn't happen on the live daemon) defaults to abort
        // — never run a call we couldn't get a decision for.
        rx.await.unwrap_or(Approval::Abort)
    }

    async fn interrupted(&mut self) {
        self.shared.interrupt.notified().await;
    }
}

/// The notification a **session-visible** config edit raises (backlog/permissions/config-edit.md),
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

#[derive(Deserialize)]
struct TurnBody {
    text: String,
}

#[derive(Deserialize)]
struct DecisionBody {
    decision: String,
    #[serde(default)]
    message: Option<String>,
    /// The config-edit payload (`decision: "config_edit"` only) — the change, its lifetime
    /// (`session`/`persist`, default `session`), and a rationale note (backlog/permissions/config-edit.md).
    #[serde(default)]
    change: Option<crate::log::ConfigChange>,
    #[serde(default)]
    lifetime: Option<crate::log::Lifetime>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct NotifyBody {
    source: String,
    body: String,
    /// Logical channel — a later notify on the same slot supersedes this one (docs/inbox.md).
    #[serde(default)]
    slot: Option<String>,
    /// Supersede a specific pending notification by its queue-local id.
    #[serde(default)]
    replaces: Option<u64>,
}

/// Pull the shared daemon state out of a conn's server-global TypeSet, or short
/// out with a 500 if it's somehow absent (a wiring bug, never seen at runtime).
fn shared_of(conn: &Conn) -> Option<Arc<Shared>> {
    conn.shared_state::<Arc<Shared>>().map(Arc::clone)
}

/// The HTTP surface — a `trillium-router` over the verb set (docs/control-channel.md). Each
/// handler reads `Shared` from the server-global state; JSON in, JSON out. The
/// router gives method-aware dispatch (and a clean `:call_id` capture) in place
/// of the earlier hand-rolled `(method, path)` match.
fn router() -> Router {
    Router::new()
        // Enqueue a user turn (also seeds turn 1). 202 — watch /log (and the
        // future /events) for the response; submission is decoupled from inference.
        .post("/turn", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let wait = wants_wait(conn.querystring());
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<TurnBody>(&body) {
                Ok(TurnBody { text }) => {
                    shared.submit_turn(text);
                    // `?wait`: hold the connection until the turn reaches a gate or completes,
                    // then answer with the resulting state — "act → read state" in one round-trip
                    // (backlog/permissions/proposal-lifecycle.md).
                    if wait {
                        settle_and_state(&shared, conn).await
                    } else {
                        conn.with_status(202).with_body(r#"{"status":"accepted"}"#)
                    }
                }
                Err(e) => conn
                    .with_status(400)
                    .with_body(format!(r#"{{"error":"bad /turn body: {e}"}}"#)),
            }
        })
        // Resolve a pending approval gate by its tool_use id.
        .post("/approvals/:call_id", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let id = CallId(conn.param("call_id").unwrap_or_default().to_string());
            let wait = wants_wait(conn.querystring());
            let body = conn.request_body_string().await.unwrap_or_default();
            let decision = match serde_json::from_str::<DecisionBody>(&body) {
                Ok(d) => d,
                Err(e) => {
                    return conn
                        .with_status(400)
                        .with_body(format!(r#"{{"error":"bad decision body: {e}"}}"#));
                }
            };
            let approval = match decision.decision.as_str() {
                "approve" => Approval::Approve,
                "abort" => Approval::Abort,
                "redirect" => Approval::Redirect(decision.message.unwrap_or_default()),
                "config_edit" => match decision.change {
                    Some(change) => Approval::ConfigEdit {
                        change,
                        lifetime: decision.lifetime.unwrap_or(crate::log::Lifetime::Session),
                        note: decision.note.unwrap_or_default(),
                    },
                    None => {
                        return conn
                            .with_status(400)
                            .with_body(r#"{"error":"config_edit decision requires a change"}"#);
                    }
                },
                other => {
                    return conn
                        .with_status(400)
                        .with_body(format!(r#"{{"error":"unknown decision: {other}"}}"#));
                }
            };
            if shared.resolve_approval(&id, approval) {
                // `?wait`: hold until the turn reaches its *next* gate or completes — so a
                // multi-gate turn is driven as a clean "approve → read state" loop.
                if wait {
                    settle_and_state(&shared, conn).await
                } else {
                    conn.with_status(200).with_body(r#"{"status":"resolved"}"#)
                }
            } else {
                conn.with_status(404)
                    .with_body(r#"{"error":"no pending approval with that id"}"#)
            }
        })
        .post("/interrupt", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            shared.interrupt();
            conn.with_status(200)
                .with_body(r#"{"status":"interrupted"}"#)
        })
        // Enqueue an ambient notification into the inbox (docs/inbox.md). 202 — it waits
        // for the next triage gate (Slice C); a client watches /state for the inbox.
        .post("/notify", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<NotifyBody>(&body) {
                Ok(n) => {
                    let id = shared.notify(n.source, n.body, n.slot, n.replaces);
                    conn.with_status(202).with_body(format!(r#"{{"id":{id}}}"#))
                }
                Err(e) => conn
                    .with_status(400)
                    .with_body(format!(r#"{{"error":"bad /notify body: {e}"}}"#)),
            }
        })
        // Dismiss a pending notification out-of-band by its queue-local id (docs/inbox.md).
        .post("/notifications/:id/dismiss", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            match conn.param("id").and_then(|s| s.parse::<u64>().ok()) {
                Some(id) if shared.dismiss_notification(id) => {
                    conn.with_status(200).with_body(r#"{"status":"dismissed"}"#)
                }
                Some(_) => conn
                    .with_status(404)
                    .with_body(r#"{"error":"no pending notification with that id"}"#),
                None => conn
                    .with_status(400)
                    .with_body(r#"{"error":"notification id must be an integer"}"#),
            }
        })
        .get("/state", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            conn.with_status(200).with_body(shared.state_json())
        })
        // Long-poll: block until the daemon reaches a decision point (a gate is pending, or the
        // loop is idle), then return the `/state` snapshot. Lets a client drive the daemon
        // synchronously — "act, read state, act" — instead of polling `/state` in a wall-clock
        // loop (backlog/permissions/proposal-lifecycle.md). Returns immediately if already settled.
        .get("/settle", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            settle_and_state(&shared, conn).await
        })
        .get("/log", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let since = parse_since(conn.querystring());
            conn.with_status(200).with_body(shared.log_json(since))
        })
        // Live tail of the append-only log (docs/README.md — the log *is* the event
        // store). `?since=<seq>` replays from there then follows; default 0 streams
        // the whole session and then live. Each event carries `id: <seq>`, so a
        // reconnecting client resumes with `?since=<last_seq + 1>` — the explicit
        // cursor the SSE crate's bare framing leaves to us.
        .get("/events", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let since = parse_since(conn.querystring()).unwrap_or(0);
            conn.with_sse_stream(tail_events(shared, since))
        })
        // Live tail of *ephemeral* output deltas (docs/README.md): assistant prose and
        // tool output as they're produced, before the turn commits. No `since`
        // cursor and no replay — these carry no `seq` and aren't durable; a client
        // that wants the authoritative record tails `/events`. A late subscriber
        // simply starts from the next delta.
        .get("/stream", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            conn.with_sse_stream(stream_deltas(shared))
        })
}

/// One log entry as a Server-Sent Event. We implement [`Eventable`] ourselves
/// rather than use `trillium_sse::Event` because that struct exposes no `id`
/// setter, and `id: <seq>` is the whole point — it's the resume cursor.
struct LogEvent {
    /// The entry's `seq`, stringified so `id()` can hand back a `&str`.
    id: String,
    /// The payload kind → the SSE `event:` field (`user` / `assistant` /
    /// `tool_result`), so clients can switch without parsing the body first.
    kind: &'static str,
    /// The full serialized [`LogEntry`] — the same JSON `GET /log` returns, so the
    /// tail and the catch-up read are byte-identical per entry.
    data: String,
}

impl Eventable for LogEvent {
    fn data(&self) -> &str {
        &self.data
    }
    fn event_type(&self) -> Option<&str> {
        Some(self.kind)
    }
    fn id(&self) -> Option<&str> {
        Some(&self.id)
    }
}

fn log_event(entry: &LogEntry) -> LogEvent {
    let kind = match &entry.payload {
        Payload::User(_) => "user",
        Payload::Assistant { .. } => "assistant",
        Payload::ToolResult { .. } => "tool_result",
        Payload::Mark(_) => "mark",
        Payload::Triage(_) => "triage",
        Payload::Ambient(_) => "ambient",
        Payload::Spot(_) => "spot",
        Payload::ConfigEdit(_) => "config_edit",
    };
    LogEvent {
        id: entry.seq.to_string(),
        kind,
        data: serde_json::to_string(entry).unwrap_or_default(),
    }
}

/// A per-connection stream that emits every log entry from `since` onward and
/// then follows new appends live (docs/control-channel.md). This is the fanout
/// `trillium-sse` deliberately doesn't provide — and tailing the durable log
/// (rather than a broadcast channel) means a slow client never drops events: the
/// log *is* the buffer, and each connection reads at its own pace.
///
/// Boxed because `with_sse_stream` demands `Unpin + Send + Sync`: the raw
/// `unfold` over an async block is `!Unpin`, and `futures-lite`'s `.boxed()` adds
/// only `+ Send`, so we box to the `+ Send + Sync` trait object by hand.
fn tail_events(
    shared: Arc<Shared>,
    since: u64,
) -> Pin<Box<dyn Stream<Item = LogEvent> + Send + Sync + 'static>> {
    Box::pin(futures_lite::stream::unfold(
        (shared, since),
        |(shared, next)| async move {
            loop {
                // Arm the wait BEFORE reading the log. tokio guarantees a
                // `notify_waiters()` reaches any `Notified` created before the
                // call, so an append landing between our read and our await can't
                // be lost (no `enable()` needed for the `notify_waiters` path).
                let waiter = Arc::clone(&shared);
                let appended = waiter.appended.notified();
                if let Some(entry) = shared.entry_at(next) {
                    return Some((log_event(&entry), (shared, next + 1)));
                }
                appended.await;
            }
        },
    ))
}

/// One ephemeral [`StreamDelta`] as an SSE event. **No `id`** — deltas are
/// pre-commit and carry no `seq`; that absence is the whole reason they ride a
/// side channel rather than `/events`, whose `id == seq` is a durable resume
/// cursor. The body is always JSON so a fragment containing newlines can't perturb
/// SSE's line-oriented `data:` framing.
struct DeltaEvent {
    /// The delta kind → the SSE `event:` field, so a client switches without
    /// parsing the body first (`assistant_delta` / `tool_output` / `bg_output`).
    kind: &'static str,
    data: String,
}

impl Eventable for DeltaEvent {
    fn data(&self) -> &str {
        &self.data
    }
    fn event_type(&self) -> Option<&str> {
        Some(self.kind)
    }
    fn id(&self) -> Option<&str> {
        None
    }
}

fn delta_event(delta: &StreamDelta) -> DeltaEvent {
    match delta {
        StreamDelta::AssistantText(text) => DeltaEvent {
            kind: "assistant_delta",
            data: serde_json::json!({ "text": text }).to_string(),
        },
        StreamDelta::ToolOutput { call_id, chunk } => DeltaEvent {
            kind: "tool_output",
            data: serde_json::json!({ "call_id": call_id, "chunk": chunk }).to_string(),
        },
        StreamDelta::BgOutput { handle, chunk } => DeltaEvent {
            kind: "bg_output",
            data: serde_json::json!({ "handle": handle, "chunk": chunk }).to_string(),
        },
    }
}

/// A per-connection stream of ephemeral output deltas (docs/README.md). Subscribes to
/// the broadcast and forwards each delta as an SSE event. A consumer that lags past
/// the ring drops the oldest deltas (`Lagged`) and continues — acceptable precisely
/// because the durable log (`/events`) is the authoritative backstop, so the stream
/// trades completeness for never stalling the producing loop.
fn stream_deltas(
    shared: Arc<Shared>,
) -> Pin<Box<dyn Stream<Item = DeltaEvent> + Send + Sync + 'static>> {
    let rx = shared.subscribe_stream();
    Box::pin(futures_lite::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(delta) => return Some((delta_event(&delta), rx)),
                // Fell behind the ring: skip the gap and keep tailing live.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                // All senders dropped (daemon tearing down): end the stream.
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }))
}

/// The detached drain task for one backgrounded process (docs/supervisor.md): feed every
/// output chunk into the registry's capture buffer until the child exits (EOF —
/// reached naturally, or because a kill closed the pipes), then mark it exited and
/// fire its exit notification into the inbox, where the next triage gate adjudicates
/// it like any notification. The supervisor lock is taken per chunk and never held
/// across an await.
async fn drain_background(
    shared: Arc<Shared>,
    handle: String,
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
) {
    crate::bash::drain_pipes(stdout, stderr, std::future::pending(), |chunk| {
        shared.supervisor.lock().unwrap().push_chunk(&handle, chunk);
        // Smooth the live process panel between `/state` polls (TUI-bg-2). Lossy
        // and best-effort — the poll snapshot remains the authoritative tail.
        shared.publish_delta(StreamDelta::BgOutput {
            handle: handle.clone(),
            chunk: String::from_utf8_lossy(chunk).into_owned(),
        });
    })
    .await;
    let status = child.wait().await.ok();
    let summary = shared.supervisor.lock().unwrap().finish(&handle, status);
    if let Some((source, body)) = summary {
        shared.notify(source, body, Some(handle), None);
    }
}

const MISSING_STATE: &str = r#"{"error":"missing shared state"}"#;

/// The fs-watch initiator (backlog/permissions/proposal-lifecycle.md): watch the config file and
/// turn external edits into **committed** `ConfigEdit`s. A human editing the file *is* the
/// orchestrator committing, so each diff lands directly as `Committed` (not a proposal) and folds
/// into the effective policy live — no restart.
///
/// We watch the **parent directory** and filter to the filename, not the file inode: editors
/// save-via-rename (write a temp, rename over the original), which orphans an inode-level watch.
/// Events are debounced (one save fires several raw events). The harness's own auto-persist
/// write-back echoes back through here harmlessly — [`config::diff_changes`] is additive and the
/// file already matches the effective config, so the echo diffs to nothing.
///
/// The debouncer runs `notify` on its own thread and invokes the handler there; we forward each
/// batch into this async task over a channel, and the task **owns** the debouncer so watching lives
/// exactly as long as the task (the `Debouncer` guard stops on drop).
async fn config_watch_loop(shared: Arc<Shared>, path: PathBuf) {
    // A relative `efference.toml` has an empty parent; watch cwd in that case.
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let target = path.file_name().map(|n| n.to_os_string());

    let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(Duration::from_millis(400), None, move |res| {
        // The handler runs on notify's thread — just hand the batch to the async side.
        let _ = tx.send(res);
    }) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[config-watch: could not start watcher for {}: {e}]",
                path.display()
            );
            return;
        }
    };
    if let Err(e) = debouncer.watch(&dir, RecursiveMode::NonRecursive) {
        eprintln!("[config-watch: could not watch {}: {e}]", dir.display());
        return;
    }
    eprintln!("watching config: {}", path.display());

    while let Some(result) = rx.recv().await {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                for e in errors {
                    eprintln!("[config-watch error: {e}]");
                }
                continue;
            }
        };
        // We watch the whole dir; react only to events that touch our file. Compare by file name —
        // robust to relative/absolute path differences in how the backend reports the event path.
        let touches = events.iter().any(|ev| {
            ev.event
                .paths
                .iter()
                .any(|p| p.file_name().map(|n| n.to_os_string()) == target)
        });
        if touches {
            reload_and_emit(&shared, &path);
        }
    }
}

/// Reload the config file and emit a `Committed` `ConfigEdit` per additive/replace delta against the
/// current effective policy (backlog/permissions/proposal-lifecycle.md). A reload failure (malformed
/// edit mid-save, a transient delete during rename) is logged and skipped — we keep the last good
/// policy rather than failing the daemon. Diffing against *effective* means only genuinely-new
/// deltas surface, so the harness's own persist write-back is a no-op echo.
fn reload_and_emit(shared: &Shared, path: &std::path::Path) {
    let file_config = match crate::config::load(Some(path)) {
        Ok(loaded) => loaded.role,
        Err(e) => {
            eprintln!("[config-watch: reload failed, keeping current policy: {e}]");
            return;
        }
    };
    let changes = crate::config::diff_changes(&shared.effective_config(), &file_config);
    if changes.is_empty() {
        return;
    }
    let turn = shared.latest_turn();
    for change in changes {
        // The file edit *is* the commit, and it lives in the file by definition → `Persist` (the
        // audit answer to "survives a reboot?" is yes). It does not go through `commit_config_edit`,
        // so it is *not* written back — that would loop against the very write that triggered us.
        let edit = crate::log::ConfigEdit {
            state: EditState::Committed,
            resolves: None,
            change,
            lifetime: crate::log::Lifetime::Persist,
            note: "config file edited".into(),
        };
        if let Err(e) = shared.append(turn, Payload::ConfigEdit(edit)) {
            eprintln!("[config-watch: append failed: {e}]");
        }
    }
}

/// The cache keepalive (docs/substrate.md): a daemon-side timer that fires a
/// `max_tokens: 0` ping when the canonical prefix nears its 5-minute cache TTL
/// during idle, refreshing it so the next real turn resumes hot instead of cold.
/// Pure transport — it reads the prefix and records the usage; the log is never
/// touched.
///
/// Two clocks ([`Shared::keepalive_clocks`]): ping when the cache-touch clock
/// nears the TTL, but stop once the *real-turn* clock passes `CAP` — past that the
/// human has been gone long enough that pinging forever costs more than one cold
/// read on resume (the crossover, docs/substrate.md). 5m TTL only: a warm cache can't be entered
/// at 1h (verified — TTL is write-time), so 1h is a future cold-write knob, not a
/// keepalive concern.
async fn keepalive_loop(client: Arc<Client>, shared: Arc<Shared>, tools: Vec<Value>) {
    const CHECK: Duration = Duration::from_secs(20);
    const PING_AT: Duration = Duration::from_secs(240); // 4 min — margin under the 5m TTL
    const CAP: Duration = Duration::from_secs(30 * 60); // give up after 30 min idle
    loop {
        tokio::time::sleep(CHECK).await;
        // Need a real turn to have happened (a prefix worth keeping warm) and to be
        // inside the window: nearing eviction but not yet past the give-up cap.
        let (Some(touch), Some(turn)) = shared.keepalive_clocks() else {
            continue;
        };
        if turn >= CAP || touch < PING_AT {
            continue;
        }
        // Same prefix a real turn would send (byte-identical, or it writes a new
        // entry instead of refreshing the live one); `max_tokens: 0` reads it.
        let mut messages = shared.project_messages();
        place_trailing_cache_breakpoint(&mut messages);
        match client
            .keepalive(&messages, &tools, ToolChoice::auto())
            .await
        {
            Ok(usage) => {
                eprintln!(
                    "[keepalive: cache read {} — TTL refreshed]",
                    usage.cache_read_input_tokens
                );
                shared.record_keepalive(usage);
            }
            Err(e) => eprintln!("[keepalive failed: {e}]"),
        }
    }
}

/// Pull `since=<n>` out of a raw query string (`GET /log?since=12`).
fn parse_since(query: &str) -> Option<u64> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("since="))
        .and_then(|v| v.parse().ok())
}

/// Whether a request asked to block until settled (`?wait`, `?wait=1`, `?wait=true`) — the
/// long-poll opt-in on `/turn` and `/approvals` (backlog/permissions/proposal-lifecycle.md).
fn wants_wait(query: &str) -> bool {
    query
        .split('&')
        .any(|kv| matches!(kv, "wait" | "wait=1" | "wait=true"))
}

/// Cap on a `/settle` (or `?wait`) long-poll: past this, return the current state regardless, so a
/// wedged loop can never hold a client forever. Far longer than any real turn — a clean settle wins
/// the race in practice; this is only the backstop.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Block until the daemon settles (capped by [`SETTLE_TIMEOUT`]), then answer with the `/state`
/// snapshot — the shared tail of `GET /settle` and the `?wait` paths. The body is exactly `/state`'s,
/// so a settled response is just a fresh state read; the client inspects `pending_approval` to tell a
/// gate from idle.
async fn settle_and_state(shared: &Arc<Shared>, conn: Conn) -> Conn {
    let _ = tokio::time::timeout(SETTLE_TIMEOUT, shared.await_settled()).await;
    conn.with_status(200).with_body(shared.state_json())
}

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
) -> Result<(), Error> {
    let (shared, turns) = Shared::with_role(log, role);
    // Share the client between the loop and the keepalive task. `tools` is computed
    // once here for the ping; `agent::run` recomputes the same (byte-stable, docs/substrate.md)
    // definitions internally, so both send an identical prefix.
    let client = Arc::new(client);
    // The exact tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the supervisor/inbox tools, docs/supervisor.md, docs/inbox.md). Built from the one source
    // (`agent::tool_definitions`) so the keepalive ping warms the *same* cache prefix
    // a real turn reads; any drift here silently breaks idle cache-warming (docs/substrate.md).
    let tools = agent::tool_definitions(&toolbox);
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns,
    };

    // `with_shared_state` carries the Arc into every conn's TypeSet; `listeners()`
    // upgrades to the multi-listener builder (carrying that state over) so we can
    // `bind_uds`. The same builder's `bind_tcp`/`bind_env` is the deferred-TCP
    // seam (docs/transport.md) — a config switch, not an abstraction to build.
    let server = trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(router());
    eprintln!("control socket: {}", socket.display());

    // The cache keepalive runs alongside the loop (docs/substrate.md) — a transport-side
    // timer that never touches the log, so it composes with the loop without
    // sharing anything but the read-only client and the telemetry clocks.
    tokio::spawn(keepalive_loop(
        Arc::clone(&client),
        Arc::clone(&shared),
        tools,
    ));

    // The fs-watch initiator (backlog/permissions/proposal-lifecycle.md): if the config resolved
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client as ControlClient;
    use crate::log::ContentBlock;
    use futures_lite::StreamExt;
    use std::time::Duration;

    fn user(text: &str) -> Payload {
        Payload::User(vec![ContentBlock::Text(text.into())])
    }

    /// `GET /state` reports cache telemetry: the latest `usage`, running totals,
    /// and a seconds-since stamp. Drives the cache gate signature through it (docs/README.md) —
    /// a cold first request (all creation) then a hot one (reads climb, creation
    /// collapses) — and asserts the surface reflects both. Transport-only: the
    /// log is untouched, so this never perturbs projection or byte-stability.
    #[test]
    fn state_reports_cache_telemetry() {
        let log_path = std::env::temp_dir().join("efference-usage-test.jsonl");
        let _ = std::fs::remove_file(&log_path);
        let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));

        // No request yet: no `last`, no stamp.
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        assert!(v["usage"]["last"].is_null());
        assert!(v["usage"]["seconds_since_last_request"].is_null());

        // Cold prefix: all creation, no read.
        shared.record_request(Usage {
            input_tokens: 6,
            output_tokens: 10,
            cache_creation_input_tokens: 9407,
            cache_read_input_tokens: 0,
        });
        // Now hot: reads climb, creation collapses (the cache gate signature, docs/README.md).
        shared.record_request(Usage {
            input_tokens: 6,
            output_tokens: 12,
            cache_creation_input_tokens: 165,
            cache_read_input_tokens: 9407,
        });

        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        // `last` is the most recent request.
        assert_eq!(v["usage"]["last"]["cache_read_input_tokens"], 9407);
        assert_eq!(v["usage"]["last"]["cache_creation_input_tokens"], 165);
        // Cumulative sums across both.
        assert_eq!(
            v["usage"]["cumulative"]["cache_creation_input_tokens"],
            9407 + 165
        );
        assert_eq!(v["usage"]["cumulative"]["cache_read_input_tokens"], 9407);
        assert_eq!(v["usage"]["cumulative"]["output_tokens"], 22);
        // A wall-clock stamp now exists — the eviction-prediction signal (docs/substrate.md).
        assert!(v["usage"]["seconds_since_last_request"].is_u64());
    }

    /// `GET /state` reports the resolved role (backlog/permissions/README.md): a
    /// `null` source for the built-in default, and the path plus the **effective** config's full
    /// contents (scope, watch-for, commands, tools, paths) when a config is in effect — so "what is
    /// my live perimeter?" is answerable, not just "which file?". The contents track the *effective*
    /// policy, so a folded config edit shows up (backlog/permissions/proposal-lifecycle.md).
    #[test]
    fn state_reports_effective_role_contents() {
        // Default role (no config file) → null source, empty everywhere.
        let (shared, _rx) = Shared::new(Log::in_memory());
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        assert!(v["role"]["source"].is_null());
        assert!(v["role"]["scope"].is_null());
        assert_eq!(v["role"]["commands"], serde_json::json!([]));

        // A loaded config surfaces its resolved path and full contents (sorted, by BTree order).
        let loaded = LoadedConfig {
            role: crate::config::Config {
                scope: Some("stay in the repo".into()),
                watch_for: vec!["network egress".into()],
                commands: ["cargo", "git diff"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                tools: ["read"].iter().map(|s| s.to_string()).collect(),
                paths: [(
                    ".".to_string(),
                    crate::config::Perms {
                        read: true,
                        write: true,
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            source: ConfigSource::Path("efference.toml".into()),
        };
        let (shared, _rx) = Shared::with_role(Log::in_memory(), loaded);
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        assert_eq!(v["role"]["source"], "efference.toml");
        assert_eq!(v["role"]["scope"], "stay in the repo");
        assert_eq!(
            v["role"]["watch_for"],
            serde_json::json!(["network egress"])
        );
        assert_eq!(
            v["role"]["commands"],
            serde_json::json!(["cargo", "git diff"])
        );
        assert_eq!(v["role"]["tools"], serde_json::json!(["read"]));
        assert_eq!(
            v["role"]["paths"]["."],
            serde_json::json!({ "read": true, "write": true })
        );

        // The fix: a *committed* config edit moves the reported perimeter (the boot config did not
        // list `npm`; after the edit, `/state` does) — `/state` reflects effective, not base.
        shared.append(0, allow_grant("npm")).unwrap();
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        assert_eq!(
            v["role"]["commands"],
            serde_json::json!(["cargo", "git diff", "npm"])
        );
    }

    /// One bash `ToolCall`, for exercising the effective-policy cache through `classify`.
    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            id: CallId("toolu_x".into()),
            name: crate::bash::NAME.into(),
            input: serde_json::json!({ "command": command }),
        }
    }

    fn allow_grant(program: &str) -> Payload {
        committed_edit(crate::log::ConfigChange::Command {
            command: program.into(),
        })
    }

    /// A committed config edit (the synchronous/direct commit path) as a `Payload` — no prior
    /// proposal to resolve.
    fn committed_edit(change: crate::log::ConfigChange) -> Payload {
        Payload::ConfigEdit(crate::log::ConfigEdit {
            state: EditState::Committed,
            resolves: None,
            change,
            lifetime: crate::log::Lifetime::Session,
            note: String::new(),
        })
    }

    /// The effective-policy cache updates incrementally: a `ConfigEdit` appended mid-session
    /// promotes a call the base gated into the fast-path, with no log re-scan at the gate
    /// (backlog/permissions/config-edit.md).
    #[test]
    fn appending_a_config_edit_updates_the_effective_config() {
        use crate::policy::{Verdict, classify};
        let call = bash_call("npm install");

        // Empty base role → npm gates.
        let (shared, _rx) = Shared::new(Log::in_memory());
        assert_eq!(classify(&shared.effective_config(), &call), Verdict::Gate);

        // ConfigEdit `allow "npm"` → the same call now clears, via the incrementally-folded role.
        shared.append(0, allow_grant("npm")).unwrap();
        assert_eq!(classify(&shared.effective_config(), &call), Verdict::Clear);
    }

    /// A session-visible config edit (`scope`, `paths`) also raises a pending notification so the
    /// session learns its perimeter moved; a classifier-only edit (`commands`/`tools`) does not
    /// (backlog/permissions/config-edit.md).
    #[test]
    fn session_visible_config_edit_enqueues_a_notification() {
        let edit = committed_edit;

        // A scope edit notifies.
        let (shared, _rx) = Shared::new(Log::in_memory());
        assert!(!shared.has_pending_notifications());
        shared
            .append(
                0,
                edit(crate::log::ConfigChange::Scope {
                    text: "stay in src/".into(),
                }),
            )
            .unwrap();
        assert!(
            shared.has_pending_notifications(),
            "scope edit is session-visible"
        );

        // A path edit notifies.
        let (shared, _rx) = Shared::new(Log::in_memory());
        shared
            .append(
                0,
                edit(crate::log::ConfigChange::Path {
                    path: ".".into(),
                    read: true,
                    write: true,
                }),
            )
            .unwrap();
        assert!(
            shared.has_pending_notifications(),
            "path edit is session-visible"
        );

        // A command edit does not — its effect reaches the model only through the classifier.
        let (shared, _rx) = Shared::new(Log::in_memory());
        shared.append(0, allow_grant("npm")).unwrap();
        assert!(
            !shared.has_pending_notifications(),
            "command edit is classifier-only"
        );
    }

    /// A `Proposed` edit as a `Payload`, for driving the lifecycle in tests.
    fn proposed_edit(change: crate::log::ConfigChange) -> Payload {
        Payload::ConfigEdit(crate::log::ConfigEdit {
            state: EditState::Proposed,
            resolves: None,
            change,
            lifetime: crate::log::Lifetime::Session,
            note: String::new(),
        })
    }

    /// The proposal lifecycle (backlog/permissions/proposal-lifecycle.md): a `Proposed` edit is
    /// **inert** in the effective config but lands in the pending-proposals queue, addressed by its
    /// own `seq`; an orchestrator's `Committed` decision (referencing that `seq` via `resolves`)
    /// both applies the change *and* drains the proposal from the queue.
    #[test]
    fn a_proposal_is_inert_until_committed_then_drains_the_queue() {
        use crate::policy::{Verdict, classify};
        let call = bash_call("npm install");
        let (shared, _rx) = Shared::new(Log::in_memory());

        // Propose: inert in policy, but queued.
        let proposal_seq = shared
            .append(
                0,
                proposed_edit(crate::log::ConfigChange::Command {
                    command: "npm".into(),
                }),
            )
            .unwrap();
        assert_eq!(
            classify(&shared.effective_config(), &call),
            Verdict::Gate,
            "a proposal must not take effect",
        );
        let queued = shared.pending_proposals();
        assert_eq!(queued.len(), 1);
        assert_eq!(
            queued[0].seq, proposal_seq,
            "the proposal is addressed by its own seq"
        );

        // Commit it (referencing the proposal): the change applies and the queue drains.
        shared
            .append(
                0,
                Payload::ConfigEdit(crate::log::ConfigEdit {
                    state: EditState::Committed,
                    resolves: Some(proposal_seq),
                    change: crate::log::ConfigChange::Command {
                        command: "npm".into(),
                    },
                    lifetime: crate::log::Lifetime::Session,
                    note: String::new(),
                }),
            )
            .unwrap();
        assert_eq!(classify(&shared.effective_config(), &call), Verdict::Clear);
        assert!(
            shared.pending_proposals().is_empty(),
            "commit drains the proposal"
        );
    }

    /// The fs-watch initiator's core (backlog/permissions/proposal-lifecycle.md): reloading an
    /// edited config file emits a `Committed` edit per delta against effective policy, folding it
    /// in live; a session-visible delta (`scope`) also notifies; re-running against the *same* file
    /// is a no-op echo (the diff is empty). Tests `reload_and_emit` directly — the synchronous core
    /// the notify thread drives — so there's no filesystem-event timing in the test.
    #[test]
    fn reload_emits_committed_edits_for_file_deltas() {
        use crate::policy::{Verdict, classify};
        let path =
            std::env::temp_dir().join(format!("efference-watch-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "scope = \"stay in src/\"\n[commands]\ncargo = true\n",
        )
        .unwrap();

        let (shared, _rx) = Shared::new(Log::in_memory());
        assert_eq!(
            classify(&shared.effective_config(), &bash_call("cargo build")),
            Verdict::Gate
        );

        // First reload folds the file's grants into effective policy and notifies on the scope.
        reload_and_emit(&shared, &path);
        assert_eq!(
            classify(&shared.effective_config(), &bash_call("cargo build")),
            Verdict::Clear
        );
        assert_eq!(
            shared.effective_config().scope.as_deref(),
            Some("stay in src/")
        );
        assert!(
            shared.has_pending_notifications(),
            "scope delta is session-visible"
        );

        // A second reload against the unchanged file diffs to nothing — the idempotent echo.
        let before = shared.log.lock().unwrap().entries().len();
        reload_and_emit(&shared, &path);
        assert_eq!(
            shared.log.lock().unwrap().entries().len(),
            before,
            "no-op echo appends nothing"
        );

        std::fs::remove_file(&path).unwrap();
    }

    /// A `Cancelled` decision drains the proposal from the queue and never touches policy.
    #[test]
    fn cancelling_a_proposal_drains_the_queue_without_applying() {
        use crate::policy::{Verdict, classify};
        let call = bash_call("npm install");
        let (shared, _rx) = Shared::new(Log::in_memory());

        let proposal_seq = shared
            .append(
                0,
                proposed_edit(crate::log::ConfigChange::Command {
                    command: "npm".into(),
                }),
            )
            .unwrap();
        assert_eq!(shared.pending_proposals().len(), 1);

        shared
            .append(
                0,
                Payload::ConfigEdit(crate::log::ConfigEdit {
                    state: EditState::Cancelled,
                    resolves: Some(proposal_seq),
                    change: crate::log::ConfigChange::Command {
                        command: "npm".into(),
                    },
                    lifetime: crate::log::Lifetime::Session,
                    note: String::new(),
                }),
            )
            .unwrap();
        assert!(
            shared.pending_proposals().is_empty(),
            "cancel drains the proposal"
        );
        assert_eq!(
            classify(&shared.effective_config(), &call),
            Verdict::Gate,
            "a cancelled proposal never takes effect",
        );
    }

    /// The `/settle` predicate (backlog/permissions/proposal-lifecycle.md) tracks the loop's
    /// lifecycle: idle → busy on submit (even before dequeue) → busy mid-turn → *settled* at a gate
    /// → busy after the gate resolves → settled again when the loop parks.
    #[test]
    fn settle_predicate_tracks_the_loop_lifecycle() {
        let (shared, _rx) = Shared::new(Log::in_memory());
        assert!(shared.is_settled(), "a fresh daemon is idle");

        // Submitted but not yet dequeued — busy (the submit-race guard via the queued count).
        shared.submit_turn("hi".into());
        assert!(
            !shared.is_settled(),
            "a queued turn is busy before the loop sees it"
        );

        // Dequeued, running mid-turn — still busy.
        shared.dequeued_turn();
        assert!(!shared.is_settled(), "mid-turn is busy");

        // A gate is a decision point — settled.
        let call = bash_call("npm install");
        let _gate = shared.register_pending(&call);
        assert!(
            shared.is_settled(),
            "a pending gate is a settled decision point"
        );

        // Resolving the gate while the turn continues — busy again.
        shared.resolve_approval(&call.id, Approval::Approve);
        assert!(!shared.is_settled(), "post-gate, mid-turn is busy");

        // The loop parks — idle, settled.
        shared.parking();
        assert!(
            shared.is_settled(),
            "a parked loop with nothing queued is settled"
        );
    }

    /// `await_settled` blocks while the loop is busy and returns the moment it parks — and the
    /// `enable()`-before-check ordering means a `parking()` racing the registration isn't lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn await_settled_returns_when_the_loop_parks() {
        let (shared, _rx) = Shared::new(Log::in_memory());
        shared.submit_turn("hi".into());
        shared.dequeued_turn(); // busy, mid-turn

        let waiter = {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move { shared.await_settled().await })
        };
        // Give the waiter a chance to register and confirm it's still blocked on a busy loop.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "await_settled must block while the loop is busy"
        );

        // Parking settles it; the waiter wakes and returns.
        shared.parking();
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("await_settled should return promptly after parking")
            .expect("waiter task panicked");
    }

    /// A `config_edit` decision body decodes into its change + lifetime (lowercase wire tags),
    /// the wire shape the orchestrator posts to upgrade a gate (backlog/permissions/config-edit.md).
    #[test]
    fn config_edit_decision_body_decodes() {
        let body = r#"{"decision":"config_edit","lifetime":"persist","note":"in scope",
                       "change":{"command":{"command":"cargo nextest"}}}"#;
        let d: DecisionBody = serde_json::from_str(body).unwrap();
        assert_eq!(d.decision, "config_edit");
        assert_eq!(d.lifetime, Some(crate::log::Lifetime::Persist));
        assert_eq!(
            d.change,
            Some(crate::log::ConfigChange::Command {
                command: "cargo nextest".into()
            })
        );
    }

    /// On resume, grants already in the log fold into the *initial* effective role at
    /// construction — the policy survives a restart even before any new grant lands.
    #[test]
    fn resumed_log_edits_fold_into_the_initial_effective_config() {
        use crate::policy::{Verdict, classify};
        let mut log = Log::in_memory();
        log.append(0, allow_grant("npm")).unwrap();

        let (shared, _rx) = Shared::new(log);
        assert_eq!(
            classify(&shared.effective_config(), &bash_call("npm install")),
            Verdict::Clear
        );
    }

    /// The keepalive's load-bearing invariant (docs/substrate.md): a ping advances the
    /// cache-touch clock (it really did refresh the TTL) but must NOT reset the
    /// real-turn clock — that clock gates the give-up cap, and a ping resetting its
    /// own deadline would pin the cache forever. So after a real turn then a ping,
    /// the cap clock has been ticking strictly longer than the touch clock.
    #[test]
    fn keepalive_advances_touch_clock_not_cap_clock() {
        let log_path = std::env::temp_dir().join("efference-keepalive-test.jsonl");
        let _ = std::fs::remove_file(&log_path);
        let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));

        // Nothing to keep warm yet → both clocks absent.
        assert_eq!(shared.keepalive_clocks(), (None, None));

        let usage = Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 100,
        };
        shared.record_request(usage.clone()); // real turn → stamps both clocks
        std::thread::sleep(Duration::from_millis(5));
        shared.record_keepalive(usage); // ping → advances the touch clock only

        let (touch, turn) = shared.keepalive_clocks();
        let (touch, turn) = (touch.unwrap(), turn.unwrap());
        assert!(
            turn > touch,
            "cap clock {turn:?} must outrun the touch clock {touch:?} a ping reset"
        );
    }

    /// (id, event_type, data) of the next SSE event, with a timeout so a hung tail
    /// fails loudly instead of blocking the suite.
    async fn next_event(
        events: &mut trillium_client::sse::EventStream,
    ) -> (Option<String>, Option<String>, String) {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("an event within the timeout")
            .expect("the stream has not ended")
            .expect("the event parsed");
        (
            event.id().map(str::to_string),
            event.event_type().map(str::to_string),
            event.data().to_string(),
        )
    }

    /// `GET /events` replays the pre-existing log (id == seq) and then follows new
    /// appends live — including an append that lands *after* the client has caught
    /// up and is awaiting, which is the lost-wakeup path the tail is built to
    /// survive (the `Notified` is armed before the catch-up read).
    ///
    /// Multi-thread flavor: an SSE body streams server-write ↔ client-read
    /// concurrently, which a single-threaded test runtime can deadlock (the real
    /// daemon runs on the multi-thread runtime).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn events_replays_then_tails_live() {
        let dir = std::env::temp_dir();
        let log_path = dir.join("efference-sse-test.jsonl");
        let socket = dir.join("efference-sse-test.sock");
        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(&socket);

        let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
        // Two entries present before any client connects (the replay set).
        shared.append(1, user("first")).unwrap();
        shared.append(2, user("second")).unwrap();

        trillium_tokio::config()
            .with_shared_state(Arc::clone(&shared))
            .listeners()
            .bind_uds(&socket)
            .expect("bind uds")
            .spawn(router());

        let client = ControlClient::new(&socket);
        let mut events = client.events(Some(0)).await.expect("open events");

        // Replay: the two pre-existing entries, in order, id == seq, typed `user`.
        let (id, kind, data) = next_event(&mut events).await;
        assert_eq!(id.as_deref(), Some("0"));
        assert_eq!(kind.as_deref(), Some("user"));
        assert!(data.contains("first"));
        let (id, _, data) = next_event(&mut events).await;
        assert_eq!(id.as_deref(), Some("1"));
        assert!(data.contains("second"));

        // Live tail: append once the client is caught up and parked on the wait.
        shared.append(3, user("third")).unwrap();
        let (id, _, data) = next_event(&mut events).await;
        assert_eq!(id.as_deref(), Some("2"));
        assert!(data.contains("third"));

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&log_path);
    }

    /// `GET /stream` carries ephemeral deltas published after a client subscribes:
    /// assistant prose (`assistant_delta`) and tool output (`tool_output`), each
    /// typed, JSON-bodied, and crucially **id-less** — they're pre-commit and have
    /// no `seq`, which is the contract that keeps them off the durable `/events`
    /// cursor. The handler subscribes synchronously while building the response, so
    /// a publish after `stream()` resolves can't race ahead of the subscription.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_carries_ephemeral_deltas() {
        let dir = std::env::temp_dir();
        let log_path = dir.join("efference-stream-test.jsonl");
        let socket = dir.join("efference-stream-test.sock");
        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(&socket);

        let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
        trillium_tokio::config()
            .with_shared_state(Arc::clone(&shared))
            .listeners()
            .bind_uds(&socket)
            .expect("bind uds")
            .spawn(router());

        let client = ControlClient::new(&socket);
        let mut stream = client.stream().await.expect("open stream");

        // Published only after the subscription exists (the handler subscribed
        // before returning the response headers `stream()` just awaited).
        shared.publish_delta(StreamDelta::AssistantText("hello ".into()));
        shared.publish_delta(StreamDelta::ToolOutput {
            call_id: "toolu_5".into(),
            chunk: "line of output\n".into(),
        });
        shared.publish_delta(StreamDelta::BgOutput {
            handle: "bg#1".into(),
            chunk: "tick 3\n".into(),
        });

        // Prose delta: typed, id-less, text in a JSON body.
        let (id, kind, data) = next_event(&mut stream).await;
        assert_eq!(id, None, "deltas carry no seq");
        assert_eq!(kind.as_deref(), Some("assistant_delta"));
        let v: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["text"], "hello ");

        // Tool-output delta: tagged by the call id so a client can attribute it.
        let (id, kind, data) = next_event(&mut stream).await;
        assert_eq!(id, None);
        assert_eq!(kind.as_deref(), Some("tool_output"));
        let v: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["call_id"], "toolu_5");
        assert_eq!(v["chunk"], "line of output\n");

        // Background-process delta: tagged by handle (TUI-bg-2), demuxed like a tool.
        let (id, kind, data) = next_event(&mut stream).await;
        assert_eq!(id, None);
        assert_eq!(kind.as_deref(), Some("bg_output"));
        let v: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v["handle"], "bg#1");
        assert_eq!(v["chunk"], "tick 3\n");

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&log_path);
    }

    /// The inbox supersedes by slot (archive-and-replace, never edit — docs/inbox.md, docs/substrate.md) and
    /// dismisses by id; a superseded or dismissed entry is gone from the pending set.
    #[test]
    fn inbox_supersedes_by_slot_and_dismisses() {
        let (shared, _turns_rx) = Shared::new(Log::in_memory());
        let _a = shared.notify(
            "test-watch".into(),
            "running 5min".into(),
            Some("bash#7".into()),
            None,
        );
        let b = shared.notify("reviewer".into(), "nit at L40".into(), None, None);
        assert_eq!(shared.pending_notifications().len(), 2);

        // Same slot → replace, not append (the "running 5min" → "finished" case).
        let _c = shared.notify(
            "test-watch".into(),
            "finished".into(),
            Some("bash#7".into()),
            None,
        );
        let pending = shared.pending_notifications();
        assert_eq!(pending.len(), 2, "slot supersession replaced, didn't add");
        assert!(pending.iter().any(|n| n.body == "finished"));
        assert!(
            !pending.iter().any(|n| n.body == "running 5min"),
            "superseded entry gone"
        );

        // Explicit dismiss by id; a second dismiss of the same id is a no-op.
        assert!(shared.dismiss_notification(b));
        assert!(!shared.dismiss_notification(b));
        let pending = shared.pending_notifications();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].body, "finished");
    }

    /// A backgrounded command runs end-to-end through the supervisor (docs/supervisor.md): it
    /// spawns, its output drains into the registry, and on exit it fires an `Ambient`
    /// exit notification into the inbox — status + final tail — where a triage gate
    /// would adjudicate it. Exercises the real spawn/drain/finish/notify path with no
    /// model in the loop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_process_drains_and_notifies_on_exit() {
        let (shared, _turns_rx) = Shared::new(Log::in_memory());
        let handle = shared
            .spawn_background("echo hello; echo world 1>&2".into())
            .expect("spawn");
        assert_eq!(handle, "bg#1");

        // The detached drain task runs on the runtime; poll until the exit
        // notification lands (the command exits near-immediately).
        let mut tries = 0;
        loop {
            let pending = shared.pending_notifications();
            if let Some(n) = pending.iter().find(|n| n.source == "bg#1") {
                assert!(
                    n.body.contains("exited 0"),
                    "exit status in body: {}",
                    n.body
                );
                assert!(
                    n.body.contains("hello") || n.body.contains("world"),
                    "final tail carried: {}",
                    n.body
                );
                assert_eq!(n.slot.as_deref(), Some("bg#1"), "exit rides the bg slot");
                break;
            }
            tries += 1;
            assert!(tries < 100, "exit notification never arrived: {pending:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Exited → no longer in the ambient show or the teardown set.
        assert!(
            shared.bg_snapshot().is_empty(),
            "exited process off the peek"
        );
        shared.kill_all_bg(); // no-op (nothing running) — must not panic.
    }

    /// `GET /state` surfaces the live background processes (docs/supervisor.md) — the data the
    /// TUI process panel polls. A running child appears with its handle/command;
    /// `bg_snapshot`/`state_json` read the registry synchronously after spawn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn state_json_surfaces_running_processes() {
        let (shared, _turns_rx) = Shared::new(Log::in_memory());
        shared.spawn_background("sleep 5".into()).expect("spawn");
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        let procs = v["processes"]
            .as_array()
            .expect("processes array on /state");
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["handle"], "bg#1");
        assert_eq!(procs[0]["command"], "sleep 5");
        assert!(
            procs[0]["running_secs"].is_u64(),
            "timing present: {procs:?}"
        );
        shared.kill_all_bg();
    }

    /// `inbox_wake` (docs/inbox.md — the idle wake signal) resolves immediately when a
    /// notification is already pending, and otherwise unblocks when one arrives. Drained
    /// (shown) notifications don't re-trigger it, so a deferred item can't spin the loop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inbox_wake_fires_on_pending_not_on_drained() {
        let (shared, _turns_rx) = Shared::new(Log::in_memory());

        // Already pending → resolves at once.
        shared.notify("ci".into(), "failed".into(), None, None);
        tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
            .await
            .expect("woke on an existing pending notification");

        // Drain it (now "shown") → the queue is empty, so the wake blocks…
        let _ = shared.drain_inbox();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), shared.inbox_wake())
                .await
                .is_err(),
            "a drained notification must not keep waking the idle loop"
        );

        // …until a fresh one arrives.
        let s2 = Arc::clone(&shared);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            s2.notify("op".into(), "nudge".into(), None, None);
        });
        tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
            .await
            .expect("woke on a freshly-arrived notification");
    }

    /// The directed Slice-2 reads/acts (docs/supervisor.md, docs/inbox.md): `kill_bg` finds a running child
    /// (and rejects unknown handles), and `inbox_summary` lists pending notifications.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_bg_and_inbox_summary() {
        let (shared, _turns_rx) = Shared::new(Log::in_memory());
        assert!(!shared.kill_bg("bg#1"), "unknown handle → false");

        let h = shared.spawn_background("sleep 30".into()).expect("spawn");
        assert!(shared.kill_bg(&h), "kills a running process");

        // Inbox read: empty, then surfaces a posted notification.
        assert!(
            shared.inbox_summary().contains("empty"),
            "{}",
            shared.inbox_summary()
        );
        shared.notify("ci".into(), "build failed".into(), None, None);
        let summary = shared.inbox_summary();
        assert!(
            summary.contains("ci") && summary.contains("build failed"),
            "{summary}"
        );

        shared.kill_all_bg();
    }

    /// `POST /notify`, slot supersession, `GET /state` exposure, and
    /// `POST /notifications/:id/dismiss` over the real router + control client.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notify_supersede_and_dismiss_over_the_socket() {
        let dir = std::env::temp_dir();
        let log_path = dir.join("efference-notify-test.jsonl");
        let socket = dir.join("efference-notify-test.sock");
        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(&socket);

        let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
        trillium_tokio::config()
            .with_shared_state(Arc::clone(&shared))
            .listeners()
            .bind_uds(&socket)
            .expect("bind uds")
            .spawn(router());

        let client = ControlClient::new(&socket);
        let gone = client
            .notify("test-watch", "running 5min", Some("bash#7"), None)
            .await
            .expect("notify");
        // Same slot supersedes the first.
        client
            .notify("test-watch", "finished, 0 failures", Some("bash#7"), None)
            .await
            .expect("notify 2");

        let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
        let notes = state["pending_notifications"].as_array().unwrap();
        assert_eq!(notes.len(), 1, "slot superseded, not appended: {notes:?}");
        assert_eq!(notes[0]["body"], "finished, 0 failures");
        assert_eq!(notes[0]["source"], "test-watch");
        let live_id = notes[0]["id"].as_u64().unwrap();

        // Dismissing the live one empties the inbox; dismissing the superseded id 404s.
        client.dismiss_notification(live_id).await.unwrap();
        let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
        assert!(
            state["pending_notifications"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            client.dismiss_notification(gone).await.is_err(),
            "the superseded id is gone → 404"
        );

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&log_path);
    }
}
