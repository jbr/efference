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

use futures_lite::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, mpsc, oneshot};
use trillium::Conn;
use trillium_router::{Router, RouterConnExt};
use trillium_sse::{Eventable, SseConnExt};

use crate::agent;
use crate::api::{Client, Error};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend};
use crate::log::{CallId, LogEntry, Payload, ToolCall, Turn};
use crate::persist::Log;
use crate::project::{ApiMessage, project};
use crate::tools::Toolbox;

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
}

impl Shared {
    /// Build the shared state and hand back the turn-inbox receiver for the loop.
    pub fn new(log: Log) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (turns_tx, turns_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Self {
            log: Mutex::new(log),
            turns_tx,
            approvals: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            interrupt: Notify::new(),
            appended: Notify::new(),
        });
        (shared, turns_rx)
    }

    /// Append to the log (durably) and wake SSE tailers. The single appender is
    /// the loop; handlers only read.
    pub fn append(&self, turn: Turn, payload: Payload) -> io::Result<()> {
        self.log.lock().unwrap().append(turn, payload)?;
        self.appended.notify_waiters();
        Ok(())
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
        serde_json::json!({ "last_seq": last_seq, "pending_approval": pending }).to_string()
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

const MISSING_STATE: &str = r#"{"error":"missing shared state"}"#;

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
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns,
    };

    // `with_shared_state` carries the Arc into every conn's TypeSet; `listeners()`
    // upgrades to the multi-listener builder (carrying that state over) so we can
    // `bind_uds`. The same builder's `bind_tcp`/`bind_env` is the deferred-TCP
    // seam (DESIGN §8) — a config switch, not an abstraction to build.
    let _server = trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(router());
    eprintln!("control socket: {}", socket.display());

    agent::run(&client, &mut frontend, shared, distiller, toolbox).await
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
}
