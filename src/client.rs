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
use trillium_client::{Client as HttpClient, Connector};
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
        Self::with_connector(UnixClientConfig::new(socket))
    }

    /// Bind a client to an arbitrary [`Connector`] rather than to a socket path.
    ///
    /// Every verb below is transport-agnostic — the connector decides what "dial" means
    /// — so this is the seam a test uses to drive the control router over
    /// `trillium_testing::connector(handler)`'s in-memory transport. [`HttpClient`]
    /// type-erases its connector, so this adds no type parameter to [`Client`] or to
    /// anything holding one.
    ///
    /// The same seam is what a non-UDS binding (TCP + TLS, docs/control-api.md) would
    /// use: the API surface is identical across bindings, and only the connector differs.
    pub fn with_connector(connector: impl Connector) -> Self {
        // The connector targets a fixed endpoint, so the URL host is irrelevant — but a
        // base is still needed to form request URLs. `localhost` is a placeholder that
        // only supplies the `Host` header (UnixClientConfig docs).
        let http = HttpClient::new(connector).with_base("http://localhost/");
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
    /// (docs/permissions.md).
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
    /// "approve → read state" half of synchronous driving (docs/permissions.md).
    pub async fn approve_wait(&self, call_id: &str, decision: &Approval) -> Result<String> {
        self.post(
            &format!("/approvals/{call_id}?wait"),
            approval_body(decision),
        )
        .await
    }

    /// Block until the daemon next reaches a decision point — a gate is pending, or the loop is
    /// idle — and return the `/state` snapshot (`GET /settle`). Returns immediately if already
    /// settled (docs/permissions.md).
    pub async fn settle(&self) -> Result<String> {
        self.get("/settle").await
    }

    /// Interrupt a running tool (`POST /interrupt`) — the SIGINT equivalent that
    /// kills the tool's process group (docs/supervisor.md).
    pub async fn interrupt(&self) -> Result<()> {
        self.post("/interrupt", String::new()).await.map(drop)
    }

    /// Detach the running foreground tool into the background supervisor
    /// (`POST /background`, docs/supervisor.md) — keep it running as a `bg#<n>`
    /// process instead of killing it or waiting it out. `note`, if given, is
    /// delivered to the model as the next user message alongside the handoff.
    /// A no-op if nothing is mid-tool.
    pub async fn background(&self, note: Option<&str>) -> Result<()> {
        let body = match note {
            Some(note) => serde_json::json!({ "note": note }).to_string(),
            None => String::new(),
        };
        self.post("/background", body).await.map(drop)
    }

    /// Set the operator's pace dial / hold overlay (`POST /pace`,
    /// docs/control-channel.md). Both optional: `speed_ms` sets the between-turns
    /// delay, `held` toggles the pause. Returns the daemon's resulting `(speed_ms, held)`.
    /// Operator-view only — the agent never sees it.
    pub async fn pace(&self, speed_ms: Option<u64>, held: Option<bool>) -> Result<(u64, bool)> {
        let mut payload = serde_json::Map::new();
        if let Some(s) = speed_ms {
            payload.insert("speed_ms".into(), serde_json::json!(s));
        }
        if let Some(h) = held {
            payload.insert("held".into(), serde_json::json!(h));
        }
        let resp = self
            .post("/pace", serde_json::Value::Object(payload).to_string())
            .await?;
        let v = serde_json::from_str::<serde_json::Value>(&resp)
            .map_err(|_| anyhow!("bad /pace response: {resp}"))?;
        let speed = v.get("speed_ms").and_then(|s| s.as_u64());
        let held = v.get("held").and_then(|h| h.as_bool());
        match (speed, held) {
            (Some(s), Some(h)) => Ok((s, h)),
            _ => Err(anyhow!("no pace in /pace response: {resp}")),
        }
    }

    /// Set the operator's review-level dial (`POST /review-level`,
    /// docs/permissions.md): `autonomous` / `spotter_human` / `human_only`.
    /// Returns the daemon's resulting level.
    pub async fn review_level(&self, level: &str) -> Result<String> {
        let payload = serde_json::json!({ "level": level });
        let resp = self.post("/review-level", payload.to_string()).await?;
        serde_json::from_str::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| {
                v.get("review_level")
                    .and_then(|l| l.as_str())
                    .map(String::from)
            })
            .ok_or_else(|| anyhow!("no review_level in response: {resp}"))
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
    /// orchestrator decision (docs/permissions.md). Raw JSON array body.
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

    /// Open the unified control stream (`GET /events`, docs/control-api.md). Every event's
    /// `data` is a frame envelope discriminated by `class`; only `log` frames carry an
    /// `id`, so a reconnect resumes with `since = last_id + 1`.
    ///
    /// `since` replays the durable class from that `seq` then follows; `None` streams the
    /// whole session then live. `include` names the classes and topics wanted (`"log"`,
    /// `"log,output"`, `"state:gate"`); `None` subscribes to everything.
    pub async fn events(
        &self,
        since: Option<u64>,
        include: Option<&str>,
    ) -> Result<trillium_client::sse::EventStream> {
        let query: Vec<String> = since
            .map(|s| format!("since={s}"))
            .into_iter()
            .chain(include.map(|i| format!("include={i}")))
            .collect();
        let path = match query.is_empty() {
            true => "/events".to_string(),
            false => format!("/events?{}", query.join("&")),
        };
        self.http
            .get(path.as_str())
            .into_sse()
            .await
            .map_err(|e| anyhow!("opening the event stream at {path}: {e}"))
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
        Approval::ConfigEdit { change, note } => serde_json::json!({
            "decision": "config_edit",
            "change": change,
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
mod test;
