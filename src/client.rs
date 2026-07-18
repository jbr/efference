//! The control-channel client (docs/control-channel.md, slice 2).
//!
//! Every non-loop interaction with a running daemon goes through here: the
//! scriptable verbs (`efference turn`/`approve`/`interrupt`/`state`/`log`) and the
//! `attach` REPL are all clients of the same socket (docs/control-channel.md — interface parity,
//! there is no privileged in-process path). This is the consumer of
//! `trillium-tokio`'s [`UnixClientConfig`], the client-side Unix-socket connector
//! that unblocked this slice.
//!
//! The connector dials a *fixed* socket path, so the request URL's host/port are
//! ignored (only the path + metadata matter) — we mount a placeholder base and
//! address handlers by path alone, mirroring the daemon's hand-routed `(method,
//! path)` table.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use trillium_client::Client as HttpClient;
use trillium_tokio::UnixClientConfig;

use crate::frontend::Approval;

/// A client bound to one daemon control socket. Cheap to [`Clone`] (the inner
/// trillium client is `Arc`-backed) so `attach` can hand a copy to its
/// background event-tail task.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
}

impl Client {
    /// Bind a client to the daemon's Unix socket. Dialing happens per request via
    /// the [`UnixClientConfig`] connector; nothing connects until a verb is called.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        // The connector targets a fixed path, so the URL host is irrelevant — but a
        // base is still needed to form request URLs. `localhost` is a placeholder
        // that only supplies the `Host` header (UnixClientConfig docs).
        let http = HttpClient::new(UnixClientConfig::new(socket)).with_base("http://localhost/");
        Self { http }
    }

    /// Enqueue a user turn (`POST /turn`). Seeds turn 1 on an idle daemon. The
    /// daemon answers 202 immediately — submission is decoupled from inference, so
    /// the caller watches `/log` (or, slice 2b, `/events`) for the response.
    pub async fn turn(&self, text: &str) -> Result<()> {
        let body = serde_json::json!({ "text": text }).to_string();
        self.post("/turn", body).await.map(drop)
    }

    /// Submit a turn and **block until the daemon settles** (`POST /turn?wait`) — the turn
    /// reaches a gate or completes — returning the resulting `/state` JSON. Collapses
    /// "act → poll `/state` in a loop → read" into one round-trip
    /// (backlog/permissions/proposal-lifecycle.md).
    pub async fn turn_wait(&self, text: &str) -> Result<String> {
        let body = serde_json::json!({ "text": text }).to_string();
        self.post("/turn?wait", body).await
    }

    /// Resolve a pending approval gate (`POST /approvals/{call_id}`). The decision
    /// maps the same three-valued [`Approval`] the loop consumes: empty/approve,
    /// abort, or redirect-with-message.
    pub async fn approve(&self, call_id: &str, decision: &Approval) -> Result<()> {
        self.post(&format!("/approvals/{call_id}"), approval_body(decision))
            .await
            .map(drop)
    }

    /// Resolve a gate and **block until the daemon settles** (`POST /approvals/{id}?wait`) — the
    /// turn reaches its next gate or completes — returning the resulting `/state` JSON. The
    /// "approve → read state" half of synchronous driving (backlog/permissions/proposal-lifecycle.md).
    pub async fn approve_wait(&self, call_id: &str, decision: &Approval) -> Result<String> {
        self.post(
            &format!("/approvals/{call_id}?wait"),
            approval_body(decision),
        )
        .await
    }

    /// Block until the daemon next reaches a decision point — a gate is pending, or the loop is
    /// idle — and return the `/state` snapshot (`GET /settle`). Returns immediately if already
    /// settled (backlog/permissions/proposal-lifecycle.md).
    pub async fn settle(&self) -> Result<String> {
        self.get("/settle").await
    }

    /// Interrupt a running tool (`POST /interrupt`) — the SIGINT equivalent that
    /// kills the tool's process group (docs/supervisor.md).
    pub async fn interrupt(&self) -> Result<()> {
        self.post("/interrupt", String::new()).await.map(drop)
    }

    /// Enqueue an ambient notification into the daemon's inbox (`POST /notify`,
    /// docs/inbox.md). `slot` binds it to a logical channel so a later notify on the same
    /// slot supersedes it; `replaces` supersedes a specific pending id. Returns the
    /// new notification's queue-local id.
    pub async fn notify(
        &self,
        source: &str,
        body: &str,
        slot: Option<&str>,
        replaces: Option<u64>,
    ) -> Result<u64> {
        let mut payload = serde_json::json!({ "source": source, "body": body });
        if let Some(s) = slot {
            payload["slot"] = serde_json::json!(s);
        }
        if let Some(r) = replaces {
            payload["replaces"] = serde_json::json!(r);
        }
        let resp = self.post("/notify", payload.to_string()).await?;
        serde_json::from_str::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
            .ok_or_else(|| anyhow!("no id in /notify response: {resp}"))
    }

    /// Dismiss a pending notification out-of-band by its queue-local id
    /// (`POST /notifications/{id}/dismiss`, docs/inbox.md).
    pub async fn dismiss_notification(&self, id: u64) -> Result<()> {
        self.post(&format!("/notifications/{id}/dismiss"), String::new())
            .await
            .map(drop)
    }

    /// Current ephemeral control state (`GET /state`): the last `seq` and the one
    /// pending approval, if any. Returned as the raw JSON body.
    pub async fn state(&self) -> Result<String> {
        self.get("/state").await
    }

    /// List pending config-edit proposals (`GET /proposals`) — async edits awaiting an
    /// orchestrator decision (backlog/permissions/proposal-lifecycle.md). Raw JSON array body.
    pub async fn proposals(&self) -> Result<String> {
        self.get("/proposals").await
    }

    /// Commit a pending proposal by its `seq` (`POST /proposals/:seq`), folding its change into
    /// the effective config. `note` is the rationale for the audit record.
    pub async fn commit_proposal(&self, seq: u64, note: &str) -> Result<()> {
        let body = serde_json::json!({ "decision": "commit", "note": note }).to_string();
        self.post(&format!("/proposals/{seq}"), body)
            .await
            .map(drop)
    }

    /// Cancel a pending proposal by its `seq` (`POST /proposals/:seq`) — audit-only, drops it
    /// from the queue without touching the effective config.
    pub async fn cancel_proposal(&self, seq: u64, note: &str) -> Result<()> {
        let body = serde_json::json!({ "decision": "cancel", "note": note }).to_string();
        self.post(&format!("/proposals/{seq}"), body)
            .await
            .map(drop)
    }

    /// The durable log (`GET /log?since=<seq>`), the append-only event store
    /// (docs/README.md). Returned as the raw JSON array body.
    pub async fn log(&self, since: Option<u64>) -> Result<String> {
        let path = match since {
            Some(s) => format!("/log?since={s}"),
            None => "/log".to_string(),
        };
        self.get(&path).await
    }

    /// Open the live event stream (`GET /events?since=<seq>`): a tail of the
    /// append-only log (docs/README.md). `since` replays from that `seq` then follows;
    /// `None` streams the whole session then live. Each event's `id` is the entry
    /// `seq`, so a reconnect resumes with `since = last_id + 1`.
    pub async fn events(&self, since: Option<u64>) -> Result<trillium_client::sse::EventStream> {
        let path = match since {
            Some(s) => format!("/events?since={s}"),
            None => "/events".to_string(),
        };
        self.http
            .get(path.as_str())
            .into_sse()
            .await
            .map_err(|e| anyhow!("opening the event stream at {path}: {e}"))
    }

    /// Open the live ephemeral delta stream (`GET /stream`, docs/README.md): assistant
    /// prose and tool output as they're produced, before the turn commits. No
    /// cursor and no replay — it's a lossy live view; tail `/events` for the
    /// durable, authoritative record.
    pub async fn stream(&self) -> Result<trillium_client::sse::EventStream> {
        self.http
            .get("/stream")
            .into_sse()
            .await
            .map_err(|e| anyhow!("opening the delta stream: {e}"))
    }

    /// POST a JSON body to `path`, returning the response body on a 2xx and an
    /// error carrying the status + body otherwise.
    async fn post(&self, path: &str, body: String) -> Result<String> {
        let conn = self
            .http
            .post(path)
            .with_request_header("content-type", "application/json; charset=utf-8")
            .with_body(body)
            .await
            .with_context(|| format!("POST {path} to the control socket"))?;
        read_2xx(conn, "POST", path).await
    }

    /// GET `path`, returning the response body on a 2xx (else an error with the
    /// status + body).
    async fn get(&self, path: &str) -> Result<String> {
        let conn = self
            .http
            .get(path)
            .await
            .with_context(|| format!("GET {path} from the control socket"))?;
        read_2xx(conn, "GET", path).await
    }
}

/// The JSON body for an approval decision — the same three-valued [`Approval`] the loop consumes
/// (empty/approve, abort, redirect-with-message) plus the config-edit transaction. Shared by the
/// blocking and non-blocking `approve` verbs.
fn approval_body(decision: &Approval) -> String {
    match decision {
        Approval::Approve => serde_json::json!({ "decision": "approve" }),
        Approval::Abort => serde_json::json!({ "decision": "abort" }),
        Approval::Redirect(message) => {
            serde_json::json!({ "decision": "redirect", "message": message })
        }
        Approval::ConfigEdit {
            change,
            lifetime,
            note,
        } => serde_json::json!({
            "decision": "config_edit",
            "change": change,
            "lifetime": lifetime,
            "note": note,
        }),
    }
    .to_string()
}

/// Read a finished client conn's body, mapping a non-2xx status into an error that
/// carries the daemon's own JSON error message (so a 404 "no pending approval"
/// surfaces verbatim at the CLI).
async fn read_2xx(mut conn: trillium_client::Conn, method: &str, path: &str) -> Result<String> {
    let ok = conn.status().is_some_and(|s| s.is_success());
    let code = conn
        .status()
        .map(|s| s.code().to_string())
        .unwrap_or_else(|| "no status".into());
    let body = conn
        .response_body()
        .read_string()
        .await
        .with_context(|| format!("reading the response to {method} {path}"))?;
    if ok {
        Ok(body)
    } else {
        Err(anyhow!("{method} {path} → {code}: {body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use trillium::Conn;
    use trillium_router::{Router, RouterConnExt};

    /// A stub daemon over a real UDS: records the bodies it received and answers
    /// the handful of paths the client drives. Proves the `UnixClientConfig`
    /// connector + request/response plumbing end to end without the live API or the
    /// real agent loop — and exercises the same `trillium-router` the daemon uses.
    /// Carried in trillium's server-global state, exactly like the real `Shared`.
    #[derive(Clone, Default)]
    struct Recorder {
        turns: Arc<Mutex<Vec<String>>>,
        approvals: Arc<Mutex<Vec<(String, String)>>>,
        interrupts: Arc<Mutex<u32>>,
    }

    fn rec_of(conn: &Conn) -> Recorder {
        conn.shared_state::<Recorder>().cloned().unwrap_or_default()
    }

    fn stub_router() -> Router {
        Router::new()
            .post("/turn", |mut conn: Conn| async move {
                let rec = rec_of(&conn);
                let body = conn.request_body_string().await.unwrap_or_default();
                rec.turns.lock().unwrap().push(body);
                conn.with_status(202).with_body(r#"{"status":"accepted"}"#)
            })
            .post("/approvals/:call_id", |mut conn: Conn| async move {
                let rec = rec_of(&conn);
                let id = conn.param("call_id").unwrap_or_default().to_string();
                // Stand in for "no such gate" so the client's non-2xx path is tested.
                if id == "missing" {
                    return conn
                        .with_status(404)
                        .with_body(r#"{"error":"no pending approval with that id"}"#);
                }
                let body = conn.request_body_string().await.unwrap_or_default();
                rec.approvals.lock().unwrap().push((id, body));
                conn.with_status(200).with_body(r#"{"status":"resolved"}"#)
            })
            .post("/interrupt", |conn: Conn| async move {
                *rec_of(&conn).interrupts.lock().unwrap() += 1;
                conn.with_status(200)
                    .with_body(r#"{"status":"interrupted"}"#)
            })
            .get("/state", |conn: Conn| async move {
                conn.with_status(200)
                    .with_body(r#"{"last_seq":3,"pending_approval":null}"#)
            })
            .get("/log", |conn: Conn| async move {
                // Echo the querystring so the test can assert `since` made the trip.
                let q = conn.querystring().to_string();
                conn.with_status(200).with_body(format!(r#"{{"q":"{q}"}}"#))
            })
    }

    /// Bind the stub on a scratch UDS, returning its path. The socket file appears
    /// synchronously on `bind_uds`, so a client may dial immediately.
    fn spawn_stub(rec: Recorder) -> PathBuf {
        let dir = std::env::temp_dir();
        // A per-test unique-ish name without Date/rand: use the Arc pointer.
        let nonce = Arc::as_ptr(&rec.turns) as usize;
        let socket = dir.join(format!("efference-client-test-{nonce}.sock"));
        let _ = std::fs::remove_file(&socket);
        trillium_tokio::config()
            .with_shared_state(rec)
            .listeners()
            .bind_uds(&socket)
            .expect("bind stub uds")
            .spawn(stub_router());
        socket
    }

    #[tokio::test]
    async fn verbs_round_trip_over_the_unix_connector() {
        let rec = Recorder::default();
        let socket = spawn_stub(rec.clone());
        let client = Client::new(&socket);

        client.turn("hello world").await.unwrap();
        client
            .approve("toolu_7", &Approval::Redirect("try the other file".into()))
            .await
            .unwrap();
        client.interrupt().await.unwrap();

        let state = client.state().await.unwrap();
        assert!(state.contains(r#""last_seq":3"#));

        let log = client.log(Some(12)).await.unwrap();
        assert!(
            log.contains("since=12"),
            "since should reach the daemon: {log}"
        );

        // The stub saw exactly what the client claims to have sent.
        let turns = rec.turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert!(turns[0].contains("hello world"));
        let approvals = rec.approvals.lock().unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].0, "toolu_7");
        assert!(approvals[0].1.contains("redirect"));
        assert!(approvals[0].1.contains("try the other file"));
        assert_eq!(*rec.interrupts.lock().unwrap(), 1);

        let _ = std::fs::remove_file(&socket);
    }

    /// Regression test for the UTF-8 mis-decode mojibake (backlog/dogfooding-friction.md):
    /// `conn.request_body_string()` — the same call the real daemon's `/turn` handler
    /// uses — decodes the body according to the request's `Content-Type` charset,
    /// defaulting away from UTF-8 when none is given. `Client::post` must pin
    /// `charset=utf-8` so non-ASCII text (em-dashes, smart quotes, accents) survives
    /// the real Unix-socket round trip byte-for-byte instead of coming out as
    /// `â€"`-style mojibake.
    #[tokio::test]
    async fn non_ascii_turn_round_trips_cleanly() {
        let rec = Recorder::default();
        let socket = spawn_stub(rec.clone());
        let client = Client::new(&socket);

        let text = "em dash — smart “quotes” — café";
        client.turn(text).await.unwrap();

        let turns = rec.turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert!(
            turns[0].contains(text),
            "non-ASCII text should survive the round trip verbatim: {:?}",
            turns[0]
        );

        let _ = std::fs::remove_file(&socket);
    }

    #[tokio::test]
    async fn non_2xx_surfaces_the_daemon_error_body() {
        let rec = Recorder::default();
        let socket = spawn_stub(rec.clone());
        let client = Client::new(&socket);

        // The stub answers /approvals/missing with a 404 + JSON error body.
        let err = client
            .approve("missing", &Approval::Approve)
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("404"), "status in error: {msg}");
        assert!(msg.contains("no pending approval"), "body in error: {msg}");

        let _ = std::fs::remove_file(&socket);
    }
}
