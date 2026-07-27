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

mod approvals;
mod background;
mod config_edits;
mod http;
mod keepalive;
mod metrics;
mod notifications;
mod pace;
mod sse;
mod views;
mod watch;

pub(crate) use watch::refresh_and_emit;

use approvals::Pending;
use config_edits::{PendingProposal, session_visible_notice};
use metrics::Metrics;
use notifications::Inbox;
use pace::Pace;

use http::router;
use keepalive::keepalive_loop;
use sse::{stream_deltas, tail_events};
use watch::config_watch_loop;

#[cfg(test)]
mod test;
