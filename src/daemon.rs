//! The control channel — the daemon and its HTTP-over-Unix-socket interface
//! (DESIGN §10 control-channel).
//!
//! This closes the §8 interface-parity gap with a stance stronger than parity:
//! **the HTTP interface is the singular, authoritative input source.** The loop's
//! only [`Frontend`] is [`RemoteFrontend`], fed by the handlers here; there is no
//! privileged in-process path. Human (TUI), driver, and subagent are all clients
//! of the same socket.
//!
//! The thesis pays off (DESIGN §1): the append-only log *is* the event store the
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_lite::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use trillium::Conn;
use trillium_router::{Router, RouterConnExt};
use trillium_sse::{Eventable, SseConnExt};

use crate::agent;
use crate::api::{Client, Error, ToolChoice, Usage};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend};
use crate::log::{CallId, LogEntry, Payload, ToolCall, Turn};
use crate::persist::Log;
use crate::project::{ApiMessage, place_trailing_cache_breakpoint, project};
use crate::tools::Toolbox;

/// A live, pre-commit output fragment streamed to clients over `GET /stream`
/// (DESIGN §10 — the deferred token-level deltas). **Ephemeral and transport-only**:
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
}

/// An in-flight approval surfaced to clients via `GET /state` (the ephemeral
/// control state the log doesn't record — it stores only the *result*).
#[derive(Clone, Serialize)]
struct Pending {
    call_id: String,
    name: String,
    input: Value,
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
    /// sequential — one gate at a time, DESIGN §8).
    pending: Mutex<Option<Pending>>,
    /// Fired by `POST /interrupt`; the future `interrupted()` races a running tool.
    interrupt: Notify,
    /// Pinged on each log append; the SSE tail (next slice) will wake on it.
    appended: Notify,
    /// Fan-out for ephemeral pre-commit output deltas (DESIGN §10). Producers (the
    /// loop's `complete` sink and the bash tool) publish via [`Shared::publish_delta`];
    /// each `GET /stream` connection subscribes. Lossy by design — see [`StreamDelta`].
    stream_tx: broadcast::Sender<StreamDelta>,
    /// Cache/usage telemetry — the most recent request's `usage` and a wall-clock
    /// stamp, surfaced by `GET /state` so clients can see cache hit/miss and
    /// predict TTL eviction (DESIGN §3). Transport-only: never touches the log.
    metrics: Mutex<Metrics>,
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
    /// keepalive cap (DESIGN §3).
    last_real_turn_at: Option<Instant>,
    total_input: u64,
    total_output: u64,
    total_cache_read: u64,
    total_cache_creation: u64,
}

impl Shared {
    /// Build the shared state and hand back the turn-inbox receiver for the loop.
    pub fn new(log: Log) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (turns_tx, turns_rx) = mpsc::unbounded_channel();
        // Bounded ring: a consumer that falls this far behind drops the oldest
        // deltas (a `Lagged` on `recv`) rather than stalling the loop — the log is
        // the durable record, so a dropped delta is only a momentary gap in the
        // live view.
        let (stream_tx, _) = broadcast::channel(1024);
        let shared = Arc::new(Self {
            log: Mutex::new(log),
            turns_tx,
            approvals: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            interrupt: Notify::new(),
            appended: Notify::new(),
            stream_tx,
            metrics: Mutex::new(Metrics::default()),
        });
        (shared, turns_rx)
    }

    /// Record a completed request's `usage` and stamp the wall clock. The loop
    /// calls this once per API round-trip; `GET /state` reports the latest plus
    /// running totals and the seconds-since (the eviction-prediction signal,
    /// DESIGN §3). Transport-only — no log append, no projection change.
    pub fn record_request(&self, usage: Usage) {
        let mut m = self.metrics.lock().unwrap();
        Self::fold_usage(&mut m, usage);
        m.last_real_turn_at = Some(Instant::now());
    }

    /// A keepalive ping's usage (DESIGN §3). Folded into the telemetry and the
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
    pub fn append(&self, turn: Turn, payload: Payload) -> io::Result<()> {
        self.log.lock().unwrap().append(turn, payload)?;
        self.appended.notify_waiters();
        Ok(())
    }

    /// Publish an ephemeral pre-commit output delta to `GET /stream` subscribers
    /// (DESIGN §10). Intentionally lossy: a send with no subscribers — or to a
    /// lagging one — is dropped, because the committed entry on `/events` is the
    /// authoritative copy. Never touches the log.
    pub fn publish_delta(&self, delta: StreamDelta) {
        let _ = self.stream_tx.send(delta);
    }

    /// A fresh subscription to the delta fan-out for one `GET /stream` connection.
    fn subscribe_stream(&self) -> broadcast::Receiver<StreamDelta> {
        self.stream_tx.subscribe()
    }

    /// Project the current log into the wire (DESIGN §1). Lock held only for the
    /// synchronous fold.
    pub fn project_messages(&self) -> Vec<ApiMessage> {
        project(self.log.lock().unwrap().entries())
    }

    /// A clone of the entries — handed to the fork distiller, which awaits the
    /// model and so can't borrow across the lock (DESIGN §4).
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.log.lock().unwrap().entries().to_vec()
    }

    /// A clone of the single entry at `seq` (== its index in the log), or `None`
    /// if the log hasn't reached it yet. The `/events` tail reads one entry at a
    /// time, so it never clones the whole log.
    fn entry_at(&self, seq: u64) -> Option<LogEntry> {
        self.log.lock().unwrap().entries().get(seq as usize).cloned()
    }

    fn submit_turn(&self, text: String) {
        // The receiver lives in the loop's `RemoteFrontend`; a send fails only if
        // the loop has exited, in which case the daemon is shutting down anyway.
        let _ = self.turns_tx.send(text);
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
            "usage": usage,
        })
        .to_string()
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
        // Blocks until a `POST /turn` arrives. `None` (all senders dropped) only
        // happens once the daemon is tearing down, which ends the loop cleanly.
        self.turns.recv().await
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

#[derive(Deserialize)]
struct TurnBody {
    text: String,
}

#[derive(Deserialize)]
struct DecisionBody {
    decision: String,
    #[serde(default)]
    message: Option<String>,
}

/// Pull the shared daemon state out of a conn's server-global TypeSet, or short
/// out with a 500 if it's somehow absent (a wiring bug, never seen at runtime).
fn shared_of(conn: &Conn) -> Option<Arc<Shared>> {
    conn.shared_state::<Arc<Shared>>().map(Arc::clone)
}

/// The HTTP surface — a `trillium-router` over the verb set (DESIGN §10). Each
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
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<TurnBody>(&body) {
                Ok(TurnBody { text }) => {
                    shared.submit_turn(text);
                    conn.with_status(202).with_body(r#"{"status":"accepted"}"#)
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
                other => {
                    return conn
                        .with_status(400)
                        .with_body(format!(r#"{{"error":"unknown decision: {other}"}}"#));
                }
            };
            if shared.resolve_approval(&id, approval) {
                conn.with_status(200).with_body(r#"{"status":"resolved"}"#)
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
            conn.with_status(200).with_body(r#"{"status":"interrupted"}"#)
        })
        .get("/state", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            conn.with_status(200).with_body(shared.state_json())
        })
        .get("/log", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let since = parse_since(conn.querystring());
            conn.with_status(200).with_body(shared.log_json(since))
        })
        // Live tail of the append-only log (DESIGN §1 — the log *is* the event
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
        // Live tail of *ephemeral* output deltas (DESIGN §10): assistant prose and
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
    };
    LogEvent {
        id: entry.seq.to_string(),
        kind,
        data: serde_json::to_string(entry).unwrap_or_default(),
    }
}

/// A per-connection stream that emits every log entry from `since` onward and
/// then follows new appends live (DESIGN §10 slice 2b). This is the fanout
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
    /// parsing the body first (`assistant_delta` / `tool_output`).
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
    }
}

/// A per-connection stream of ephemeral output deltas (DESIGN §10). Subscribes to
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

const MISSING_STATE: &str = r#"{"error":"missing shared state"}"#;

/// The cache keepalive (DESIGN §3): a daemon-side timer that fires a
/// `max_tokens: 0` ping when the canonical prefix nears its 5-minute cache TTL
/// during idle, refreshing it so the next real turn resumes hot instead of cold.
/// Pure transport — it reads the prefix and records the usage; the log is never
/// touched.
///
/// Two clocks ([`Shared::keepalive_clocks`]): ping when the cache-touch clock
/// nears the TTL, but stop once the *real-turn* clock passes `CAP` — past that the
/// human has been gone long enough that pinging forever costs more than one cold
/// read on resume (the §3 crossover). 5m TTL only: a warm cache can't be entered
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
        match client.keepalive(&messages, &tools, ToolChoice::auto()).await {
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

/// Run the daemon: bind the control socket, spawn the HTTP server, and drive the
/// agent loop with a [`RemoteFrontend`]. The loop blocks until a `POST /turn`
/// arrives, so an idle daemon simply waits — retiring argv-seeding and the
/// "backgrounded run can't get its opening prompt" problem (DESIGN §10).
pub async fn serve<D: Distiller>(
    client: Client,
    toolbox: Toolbox,
    distiller: D,
    log: Log,
    socket: PathBuf,
) -> Result<(), Error> {
    let (shared, turns) = Shared::new(log);
    // Share the client between the loop and the keepalive task. `tools` is computed
    // once here for the ping; `agent::run` recomputes the same (byte-stable, §3)
    // definitions internally, so both send an identical prefix.
    let client = Arc::new(client);
    let tools = toolbox.definitions();
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns,
    };

    // `with_shared_state` carries the Arc into every conn's TypeSet; `listeners()`
    // upgrades to the multi-listener builder (carrying that state over) so we can
    // `bind_uds`. The same builder's `bind_tcp`/`bind_env` is the deferred-TCP
    // seam (DESIGN §8) — a config switch, not an abstraction to build.
    let server = trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(router());
    eprintln!("control socket: {}", socket.display());

    // The cache keepalive runs alongside the loop (DESIGN §3) — a transport-side
    // timer that never touches the log, so it composes with the loop without
    // sharing anything but the read-only client and the telemetry clocks.
    tokio::spawn(keepalive_loop(
        Arc::clone(&client),
        Arc::clone(&shared),
        tools,
    ));

    // Trillium owns the signal registry: by default it registers SIGINT/SIGTERM and
    // drives graceful shutdown through its `Swansong`. But the future `serve` awaits
    // is `agent::run`, which parks in `read_user_turn` on the turn channel — whose
    // senders live in the still-alive `Arc<Shared>`, so a C-c that stops the HTTP
    // listener would otherwise leave the loop parked forever (the `kill -9` hang).
    // Hook the *same* Swansong via `interrupt`: it drives the loop normally until
    // shutdown is initiated, then cancels it and resolves to `None`. (`server` is
    // held — not dropped — for the whole await so the listener stays up; it drops on
    // return, after shutdown has already begun.)
    let run = agent::run(&client, &mut frontend, shared, distiller, toolbox);
    match server.swansong().interrupt(run).await {
        Some(result) => result,
        None => {
            eprintln!("\nshutting down");
            Ok(())
        }
    }
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
    /// and a seconds-since stamp. Drives the §10 gate signature through it —
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
        // Now hot: reads climb, creation collapses (the §10 gate signature).
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
        // A wall-clock stamp now exists — the eviction-prediction signal (§3).
        assert!(v["usage"]["seconds_since_last_request"].is_u64());
    }

    /// The keepalive's load-bearing invariant (DESIGN §3): a ping advances the
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

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&log_path);
    }
}
