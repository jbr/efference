//! The control-channel client (docs/control-channel.md).
//!
//! Every non-loop interaction with a running daemon goes through here: the
//! `efference control <tool>` group and the MCP server that shares its definition
//! (docs/operating.md) and the TUI are all clients of the same
//! socket, with no privileged in-process path. Built on `trillium-tokio`'s
//! [`UnixClientConfig`].
//!
//! The connector dials a *fixed* socket path, so the request URL's host/port are
//! ignored (only the path + metadata matter) — we mount a placeholder base and
//! address handlers by path alone, mirroring the daemon's hand-routed `(method,
//! path)` table.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use trillium_client::{Client as HttpClient, Connector};
use trillium_tokio::UnixClientConfig;

use crate::daemon::{CreateSession, SessionOptionsRequest};
use crate::frontend::Approval;
use crate::inference::{ConnectionInventory, ResolvedOptions};

/// A client bound to one daemon control socket. Cheap to [`Clone`] (the inner
/// trillium client is `Arc`-backed) so a client can hand a copy to a
/// background event-tail task.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    /// The path the session's `/v1` routes sit under: empty at a session bind's
    /// root, `/sessions/<id>` on a host (docs/control-api.md).
    prefix: String,
}

impl Client {
    /// Bind a client to the daemon's Unix socket. Dialing happens per request via
    /// the [`UnixClientConfig`] connector; nothing connects until a verb is called.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self::with_connector(UnixClientConfig::new(socket))
    }

    /// Bind a client to one session hosted behind a prefix — `/sessions/<id>` on
    /// a host's socket (docs/control-api.md). Every `/v1` verb joins onto the
    /// prefix; the `/auth/*` family does not, since credentials are the
    /// listener's, not a session's. The prefix is normalized to a leading slash
    /// and no trailing one, so `sessions/x/` and `/sessions/x` are the same base.
    pub fn at(socket: impl Into<PathBuf>, prefix: &str) -> Self {
        Self::with_connector(UnixClientConfig::new(socket)).with_prefix(prefix)
    }

    /// Re-base this client under `prefix` (see [`at`](Self::at)); the transport
    /// is unchanged, so a test can pair it with an in-memory connector.
    pub fn with_prefix(mut self, prefix: &str) -> Self {
        let trimmed = prefix.trim_matches('/');
        self.prefix = match trimmed.is_empty() {
            true => String::new(),
            false => format!("/{trimmed}"),
        };
        self
    }

    /// Bind a client to an arbitrary [`Connector`] rather than to a socket path.
    ///
    /// Every verb below is transport-agnostic — the connector decides what "dial" means
    /// — so this is the seam a test uses to drive the control router over
    /// `trillium_testing::connector(handler)`'s in-memory transport. [`HttpClient`]
    /// type-erases its connector, so this adds no type parameter to [`Client`] or to
    /// anything holding one.
    ///
    /// The same seam carries a non-UDS binding (TCP + TLS, docs/web-bind.md): the API
    /// surface is identical across bindings, and only the connector differs.
    pub fn with_connector(connector: impl Connector) -> Self {
        // The connector targets a fixed endpoint, so the URL host is irrelevant — but a
        // base is still needed to form request URLs. `localhost` is a placeholder that
        // only supplies the `Host` header (UnixClientConfig docs).
        let http = HttpClient::new(connector).with_base("http://localhost/");
        Self {
            http,
            prefix: String::new(),
        }
    }

    /// Enqueue a user turn (`POST /turns`). Seeds turn 1 on an idle daemon. The
    /// daemon answers 202 immediately — submission is decoupled from inference, so
    /// the caller watches the stream (or `/log`) for the response.
    pub async fn turn(&self, text: &str) -> Result<()> {
        self.turn_sourced(text, None).await
    }

    /// [`turn`](Self::turn) carrying a `source` label (docs/control-api.md) — the
    /// client's self-chosen name for who is speaking (`web`, `mcp[<name>]`,
    /// `agent[<id>]`). `None` submits as the operator, who renders no prefix.
    pub async fn turn_sourced(&self, text: &str, source: Option<&str>) -> Result<()> {
        let body = Self::turn_body(text, None, source);
        self.post("/turns", body).await.map(drop)
    }

    /// Enqueue a user turn under a client-minted submission id
    /// (docs/control-api.md): the id is recorded on the resulting `User`
    /// entry and echoed in the returned 202 body, so the caller can reconcile
    /// delivery by exact key rather than content.
    pub async fn turn_with_submission_id(&self, text: &str, submission_id: &str) -> Result<String> {
        let body = Self::turn_body(text, Some(submission_id), None);
        self.post("/turns", body).await
    }

    /// The `POST /turns` body: `text`, plus `submission_id` and `source` when
    /// present. Field order is fixed here so the body is the same bytes every
    /// call for the same inputs.
    fn turn_body(text: &str, submission_id: Option<&str>, source: Option<&str>) -> String {
        let mut body = serde_json::json!({ "text": text });
        if let Some(submission_id) = submission_id {
            body["submission_id"] = serde_json::Value::from(submission_id);
        }
        if let Some(source) = source {
            body["source"] = serde_json::Value::from(source);
        }
        body.to_string()
    }

    /// Withdraw the pending-queue suffix beginning at `submission_id`
    /// (`DELETE /turns/{submission_id}`, docs/control-api.md) and return what was
    /// **actually** popped, in submission order. The loop may have dequeued the turn first, so
    /// this can return fewer turns than the queue showed — possibly none. That is the contract:
    /// a caller splicing the popped text into a composer splices only this, never a turn already
    /// on its way to the model.
    pub async fn withdraw_turns(&self, submission_id: &str) -> Result<Vec<WithdrawnTurn>> {
        let body = self.delete(&format!("/turns/{submission_id}")).await?;
        let parsed: WithdrawResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing the withdraw response: {body}"))?;
        Ok(parsed.withdrawn)
    }

    /// Submit a turn and **block until the daemon settles** (`POST /turns?wait`) — the turn
    /// reaches a gate or completes — returning the resulting `/state` JSON. Collapses
    /// "act → poll `/state` in a loop → read" into one round-trip
    /// (docs/permissions.md).
    pub async fn turn_wait(&self, text: &str) -> Result<String> {
        self.turn_wait_sourced(text, None).await
    }

    /// [`turn_wait`](Self::turn_wait) carrying a `source` label
    /// (docs/control-api.md).
    pub async fn turn_wait_sourced(&self, text: &str, source: Option<&str>) -> Result<String> {
        let body = Self::turn_body(text, None, source);
        self.post("/turns?wait", body).await
    }

    /// Resolve the pending approval gate (`PUT /approvals/{call_id}`), carrying the
    /// same four-valued [`Approval`] the loop consumes (docs/control-channel.md).
    /// The id is a precondition: addressing anything but the pending gate answers
    /// 409 `gate_moved`, with the current gate in the error's `detail`.
    pub async fn approve(&self, call_id: &str, decision: &Approval) -> Result<()> {
        self.put(&format!("/approvals/{call_id}"), approval_body(decision))
            .await
            .map(drop)
    }

    /// Resolve a gate and **block until the daemon settles** (`PUT /approvals/{id}?wait`) — the
    /// turn reaches its next gate or completes — returning the resulting `/state` JSON. The
    /// "approve → read state" half of synchronous driving (docs/permissions.md).
    pub async fn approve_wait(&self, call_id: &str, decision: &Approval) -> Result<String> {
        self.put(
            &format!("/approvals/{call_id}?wait"),
            approval_body(decision),
        )
        .await
    }

    /// Block until the daemon next reaches a decision point — a gate is pending, or the loop is
    /// idle — and return the `/state` snapshot (`GET /state?wait=settled`). Returns immediately
    /// if already settled (docs/permissions.md).
    pub async fn settle(&self) -> Result<String> {
        self.get("/state?wait=settled").await
    }

    /// Interrupt a running tool (`POST /loop/interrupt`) — the SIGINT equivalent that
    /// kills the tool's process group (docs/supervisor.md).
    /// Set the session's title (`PUT /title`, docs/session-title.md). Answers
    /// with the `session_identity` topic as it now reads.
    pub async fn set_title(&self, title: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "title": title }).to_string();
        let resp = self.put("/title", body).await?;
        serde_json::from_str(&resp).map_err(|_| anyhow!("bad /title response: {resp}"))
    }

    pub async fn interrupt(&self) -> Result<()> {
        self.post("/loop/interrupt", String::new()).await.map(drop)
    }

    /// Hand control back with no input (`POST /loop/continue`, docs/control-api.md):
    /// the idle loop takes another turn on the wire it already has. Nothing is
    /// submitted and nothing is appended, so this is not a turn with empty text —
    /// that one keeps the loop idling.
    ///
    /// A decline (`409 not_continuable`) is a normal answer, not a transport
    /// failure: the moment simply had no continue in it, and the daemon's message
    /// says which case. Errors are reserved for a request that didn't get an
    /// answer.
    pub async fn continue_loop(&self) -> Result<ContinueOutcome> {
        let path = self.versioned("/loop/continue");
        let path = path.as_str();
        let mut conn = self
            .http
            .post(path)
            .await
            .with_context(|| format!("POST {path} to the control socket"))?;
        let status = conn.status();
        let body = conn
            .response_body()
            .read_string()
            .await
            .with_context(|| format!("reading the response to POST {path}"))?;
        match status {
            Some(s) if s.is_success() => Ok(ContinueOutcome::Accepted),
            Some(trillium_client::Status::Conflict) => Ok(ContinueOutcome::Declined {
                message: json_field(&body, "message")
                    .unwrap_or_else(|_| "the daemon declined the continue".into()),
            }),
            other => {
                let code = other.map_or_else(|| "no status".to_string(), |s| s.code().to_string());
                Err(anyhow!("POST {path} → {code}: {body}"))
            }
        }
    }

    /// The running processes (`GET /processes`, docs/supervisor.md) — foreground and
    /// background in one namespace, flagged. Raw JSON array body, the same
    /// representation the `processes` state topic carries.
    pub async fn processes(&self) -> Result<String> {
        self.get("/processes").await
    }

    /// One process's retained output (`GET /processes/:id/output?since=<cursor>`,
    /// docs/control-api.md). With no cursor, everything retained — the seed a
    /// cold-opened client renders before its first `process_output` delta.
    pub async fn process_output(&self, id: u64, since: Option<u64>) -> Result<String> {
        let path = match since {
            Some(s) => format!("/processes/{id}/output?since={s}"),
            None => format!("/processes/{id}/output"),
        };
        self.get(&path).await
    }

    /// Detach the running foreground tool into the background
    /// (`PATCH /processes/:id {background: true}`, docs/supervisor.md) — keep it
    /// running as a `p#<n>` process instead of killing it or waiting it out.
    /// Resolves the one foreground entry from `GET /processes` first; backgrounding
    /// is a property change on a process that is addressable from spawn. `note`, if
    /// given, is delivered to the model as the next user message alongside the
    /// handoff. Returns the id the handoff targeted, or `None` (a no-op) when
    /// nothing is mid-tool.
    pub async fn background(&self, note: Option<&str>) -> Result<Option<u64>> {
        let processes: serde_json::Value = serde_json::from_str(&self.processes().await?)
            .map_err(|e| anyhow!("bad /processes response: {e}"))?;
        let Some(id) = processes
            .as_array()
            .and_then(|a| a.iter().find(|p| p["background"] == false))
            .and_then(|p| p["id"].as_u64())
        else {
            return Ok(None);
        };
        let mut body = serde_json::json!({ "background": true });
        if let Some(note) = note {
            body["note"] = note.into();
        }
        self.patch(&format!("/processes/{id}"), body.to_string())
            .await?;
        Ok(Some(id))
    }

    /// Kill a background process (`DELETE /processes/:id`, docs/supervisor.md) —
    /// SIGKILL to its whole process group; the drain task sees EOF and fires the
    /// exit notification like any other exit. A stale id answers 404 and is the
    /// caller's cue to drop focus from the vanished row.
    pub async fn kill_process(&self, id: u64) -> Result<()> {
        self.delete(&format!("/processes/{id}")).await.map(drop)
    }

    /// Set the operator's pace dial / hold overlay (`PATCH /loop {pace}`,
    /// docs/control-channel.md). Both optional: `speed_ms` sets the between-turns
    /// delay, `held` toggles the pause. Returns the daemon's resulting `(speed_ms, held)`.
    /// Operator-view only — the agent never sees it.
    pub async fn pace(&self, speed_ms: Option<u64>, held: Option<bool>) -> Result<(u64, bool)> {
        let mut pace = serde_json::Map::new();
        if let Some(s) = speed_ms {
            pace.insert("speed_ms".into(), serde_json::json!(s));
        }
        if let Some(h) = held {
            pace.insert("held".into(), serde_json::json!(h));
        }
        let body = serde_json::json!({ "pace": pace }).to_string();
        let resp = self.patch("/loop", body).await?;
        let v = serde_json::from_str::<serde_json::Value>(&resp)
            .map_err(|_| anyhow!("bad /loop response: {resp}"))?;
        let speed = v["pace"].get("speed_ms").and_then(|s| s.as_u64());
        let held = v["pace"].get("held").and_then(|h| h.as_bool());
        match (speed, held) {
            (Some(s), Some(h)) => Ok((s, h)),
            _ => Err(anyhow!("no pace in /loop response: {resp}")),
        }
    }

    /// Set the operator's review-level dial (`PATCH /loop {review_level}`,
    /// docs/permissions.md): `autonomous` / `upstream` / `spotter_human` / `human_only`.
    /// Returns the daemon's resulting level.
    pub async fn review_level(&self, level: &str) -> Result<String> {
        let payload = serde_json::json!({ "review_level": level });
        let resp = self.patch("/loop", payload.to_string()).await?;
        serde_json::from_str::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| {
                v.get("review_level")
                    .and_then(|l| l.as_str())
                    .map(String::from)
            })
            .ok_or_else(|| anyhow!("no review_level in response: {resp}"))
    }

    /// Enqueue an ambient notification into the daemon's inbox (`POST /notifications`,
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
        let resp = self.post("/notifications", payload.to_string()).await?;
        serde_json::from_str::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
            .ok_or_else(|| anyhow!("no id in /notifications response: {resp}"))
    }

    /// Dismiss a pending notification out-of-band by its queue-local id
    /// (`DELETE /notifications/{id}`, docs/inbox.md).
    pub async fn dismiss_notification(&self, id: u64) -> Result<()> {
        self.delete(&format!("/notifications/{id}")).await.map(drop)
    }

    /// Current ephemeral control state (`GET /state`, docs/control-api.md): the last
    /// `seq` plus one field per state topic. Returned as the raw JSON body.
    pub async fn state(&self) -> Result<String> {
        self.get("/state").await
    }

    /// List pending config-edit proposals (`GET /proposals`) — async edits awaiting an
    /// orchestrator decision (docs/permissions.md). Raw JSON array body.
    pub async fn proposals(&self) -> Result<String> {
        self.get("/proposals").await
    }

    /// Commit a pending proposal by its `seq` (`PATCH /proposals/:seq
    /// {state:"committed"}`), folding its change into the effective config. `note` is
    /// the rationale for the audit record.
    pub async fn commit_proposal(&self, seq: u64, note: &str) -> Result<()> {
        let body = serde_json::json!({ "state": "committed", "note": note }).to_string();
        self.patch(&format!("/proposals/{seq}"), body)
            .await
            .map(drop)
    }

    /// Cancel a pending proposal by its `seq` (`PATCH /proposals/:seq
    /// {state:"cancelled"}`) — audit-only, drops it from the queue without touching
    /// the effective config.
    pub async fn cancel_proposal(&self, seq: u64, note: &str) -> Result<()> {
        let body = serde_json::json!({ "state": "cancelled", "note": note }).to_string();
        self.patch(&format!("/proposals/{seq}"), body)
            .await
            .map(drop)
    }

    /// The transcript (`GET /transcript`, docs/control-api.md): a log window at
    /// reading grain, one text block per entry. `since` resumes forward, `tail`
    /// takes the last N; both together is the daemon's `400`.
    pub async fn transcript(&self, since: Option<u64>, tail: Option<u64>) -> Result<String> {
        let mut query = Vec::new();
        if let Some(since) = since {
            query.push(format!("since={since}"));
        }
        if let Some(tail) = tail {
            query.push(format!("tail={tail}"));
        }
        let path = if query.is_empty() {
            "/transcript".to_string()
        } else {
            format!("/transcript?{}", query.join("&"))
        };
        self.get(&path).await
    }

    /// One whole entry by `seq` (`GET /log/:seq`, docs/control-api.md): the full
    /// entry view, an oversized result's content included.
    pub async fn log_entry(&self, seq: u64) -> Result<String> {
        self.get(&format!("/log/{seq}")).await
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
            true => self.versioned("/events"),
            false => self.versioned(&format!("/events?{}", query.join("&"))),
        };
        self.http
            .get(path.as_str())
            .into_sse()
            .await
            .map_err(|e| anyhow!("opening the event stream at {path}: {e}"))
    }

    /// Create a session on a host (`POST /sessions`, docs/control-api.md) and
    /// return its id. A listener route, not a session's: it sits at the socket
    /// root regardless of this client's prefix, so the usual pairing is a
    /// prefix-less client for creation and [`at`](Self::at) for what follows.
    /// The request carries the caller's environment; it is written to the wire
    /// and to nothing else, and an error quotes the response, not the request.
    pub async fn create_session(&self, request: &CreateSession) -> Result<String> {
        let body = serde_json::to_string(request).context("encoding the session request")?;
        let path = "/sessions";
        let conn = self
            .http
            .post(path)
            .with_request_header("content-type", "application/json; charset=utf-8")
            .with_body(body)
            .await
            .with_context(|| format!("POST {path} to the host socket"))?;
        json_field(&read_2xx(conn, "POST", path).await?, "id")
    }

    /// Resolve a proposed hosted session (`QUERY /sessions`, docs/control-api.md)
    /// without creating a directory or starting a runtime: the effective
    /// selections, where each came from, and the issues that would stop creation.
    /// A listener route, so it always addresses the socket root.
    pub async fn session_options(
        &self,
        request: &SessionOptionsRequest,
    ) -> Result<ResolvedOptions> {
        let body = serde_json::to_string(request).context("encoding the session options")?;
        let path = "/sessions";
        let conn = self
            .http
            .build_conn(trillium::Method::Query, path)
            .with_request_header("content-type", "application/json; charset=utf-8")
            .with_body(body)
            .await
            .with_context(|| format!("QUERY {path} on the host socket"))?;
        let body = read_2xx(conn, "QUERY", path).await?;
        serde_json::from_str(&body).with_context(|| format!("decoding QUERY {path}"))
    }

    /// The host's configured inference connections (`GET /inference`). This is a
    /// listener route, so it always addresses the socket root.
    pub async fn inference_inventory(&self) -> Result<Vec<ConnectionInventory>> {
        #[derive(serde::Deserialize)]
        struct Inventory {
            connections: Vec<ConnectionInventory>,
        }
        let path = "/inference";
        let body = self.get_unversioned(path).await?;
        let inventory: Inventory =
            serde_json::from_str(&body).with_context(|| format!("decoding GET {path}"))?;
        Ok(inventory.connections)
    }

    /// The sessions a host holds (`GET /sessions`, docs/control-api.md), sorted
    /// by id, each with its title once one has been written. A listener route at
    /// the socket root, like [`create_session`](Self::create_session).
    pub async fn sessions(&self) -> Result<Vec<SessionSummary>> {
        let path = "/sessions";
        let conn = self
            .http
            .get(path)
            .await
            .with_context(|| format!("GET {path} on the host socket"))?;
        let body = read_2xx(conn, "GET", path).await?;
        serde_json::from_str(&body).with_context(|| format!("decoding GET {path}"))
    }

    /// Subscribe to the host's session listing (`GET /sessions` with
    /// `Accept: text/event-stream`, docs/control-api.md). Each event carries the
    /// whole listing — the same array [`sessions`](Self::sessions) returns — so a
    /// client holds the latest frame and needs no cursor and no reconciliation.
    /// A listener route at the socket root, like the rest of this family.
    pub async fn session_events(&self) -> Result<trillium_client::sse::EventStream> {
        let path = "/sessions";
        self.http
            .get(path)
            .into_sse()
            .await
            .map_err(|e| anyhow!("opening the session listing stream at {path}: {e}"))
    }

    /// The ids of the sessions a host holds: [`sessions`](Self::sessions) without
    /// the titles.
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        Ok(self
            .sessions()
            .await?
            .into_iter()
            .map(|listing| listing.id)
            .collect())
    }

    /// End a hosted session (`DELETE /sessions/:id`, docs/control-api.md):
    /// answers once its teardown has run. A listener route at the socket root;
    /// `404` for an id the host does not hold.
    pub async fn archive_session(&self, id: &str) -> Result<()> {
        let path = format!("/sessions/{id}");
        let conn = self
            .http
            .delete(path.as_str())
            .await
            .with_context(|| format!("DELETE {path} on the host socket"))?;
        read_2xx(conn, "DELETE", &path).await?;
        Ok(())
    }

    /// A fresh bootstrap link from the daemon's TCP bind (`GET /auth/link`,
    /// docs/web-bind.md) — current TOTP code in the fragment. The `/auth/*`
    /// credential routes answer only over the Unix socket (host possession is the
    /// enrollment authority) and live beside `/v1`, hence [`get_unversioned`].
    ///
    /// [`get_unversioned`]: Self::get_unversioned
    pub async fn auth_link(&self) -> Result<String> {
        json_field(&self.get_unversioned("/auth/link").await?, "url")
    }

    /// The authenticator-app enrollment URI (`GET /auth/otpauth`, docs/web-bind.md).
    /// Carries the TOTP secret itself — the caller renders it as a QR at the host
    /// terminal and nowhere else.
    pub async fn auth_otpauth(&self) -> Result<String> {
        json_field(&self.get_unversioned("/auth/otpauth").await?, "uri")
    }

    /// The program bearer for the TCP bind (`GET /auth/bearer`, docs/web-bind.md).
    pub async fn auth_bearer(&self) -> Result<String> {
        json_field(&self.get_unversioned("/auth/bearer").await?, "bearer")
    }

    /// GET a path that lives beside `/v1` rather than under it — the `/auth/*`
    /// family only (docs/web-bind.md).
    async fn get_unversioned(&self, path: &str) -> Result<String> {
        let conn = self
            .http
            .get(path)
            .await
            .with_context(|| format!("GET {path} from the control socket"))?;
        read_2xx(conn, "GET", path).await
    }

    /// POST a JSON body to `path`, returning the response body on a 2xx and an
    /// error carrying the status + body otherwise.
    async fn post(&self, path: &str, body: String) -> Result<String> {
        let path = self.versioned(path);
        let path = path.as_str();
        let conn = self
            .http
            .post(path)
            .with_request_header("content-type", "application/json; charset=utf-8")
            .with_body(body)
            .await
            .with_context(|| format!("POST {path} to the control socket"))?;
        read_2xx(conn, "POST", path).await
    }

    /// PATCH a JSON body to `path` — same contract as [`post`](Self::post).
    async fn patch(&self, path: &str, body: String) -> Result<String> {
        let path = self.versioned(path);
        let path = path.as_str();
        let conn = self
            .http
            .patch(path)
            .with_request_header("content-type", "application/json; charset=utf-8")
            .with_body(body)
            .await
            .with_context(|| format!("PATCH {path} to the control socket"))?;
        read_2xx(conn, "PATCH", path).await
    }

    /// PUT a JSON body to `path` — same contract as [`post`](Self::post).
    async fn put(&self, path: &str, body: String) -> Result<String> {
        let path = self.versioned(path);
        let path = path.as_str();
        let conn = self
            .http
            .put(path)
            .with_request_header("content-type", "application/json; charset=utf-8")
            .with_body(body)
            .await
            .with_context(|| format!("PUT {path} to the control socket"))?;
        read_2xx(conn, "PUT", path).await
    }

    /// DELETE `path` — same contract as [`post`](Self::post).
    async fn delete(&self, path: &str) -> Result<String> {
        let path = self.versioned(path);
        let path = path.as_str();
        let conn = self
            .http
            .delete(path)
            .await
            .with_context(|| format!("DELETE {path} on the control socket"))?;
        read_2xx(conn, "DELETE", path).await
    }

    /// One logged result image's decoded bytes (`GET /log/:seq/images/:idx`,
    /// docs/control-api.md) — the dereference target for the image references
    /// entry views carry (`data` is omitted there; this is where it lives).
    pub async fn log_image(&self, seq: u64, idx: usize) -> Result<Vec<u8>> {
        self.get_bytes(&format!("/log/{seq}/images/{idx}")).await
    }

    /// GET `path`, returning the raw response bytes on a 2xx — for the one
    /// non-JSON resource (image bytes); everything else reads through
    /// [`get`](Self::get).
    async fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let path = self.versioned(path);
        let path = path.as_str();
        let mut conn = self
            .http
            .get(path)
            .await
            .with_context(|| format!("GET {path} from the control socket"))?;
        if conn.status().is_some_and(|s| s.is_success()) {
            conn.response_body()
                .read_bytes()
                .await
                .with_context(|| format!("reading the response to GET {path}"))
        } else {
            let code = conn
                .status()
                .map(|s| s.code().to_string())
                .unwrap_or_else(|| "no status".into());
            Err(anyhow!("GET {path} → {code}"))
        }
    }

    /// GET `path`, returning the response body on a 2xx (else an error with the
    /// status + body).
    async fn get(&self, path: &str) -> Result<String> {
        let path = self.versioned(path);
        let path = path.as_str();
        let conn = self
            .http
            .get(path)
            .await
            .with_context(|| format!("GET {path} from the control socket"))?;
        read_2xx(conn, "GET", path).await
    }
}

/// One entry of `GET /sessions` (docs/control-api.md). The host defines the row
/// and both readings of the resource serve it, so there is one type on the wire
/// rather than a client copy that can drift.
pub use crate::daemon::SessionSummary;

/// One turn a `DELETE /turns/:submission_id` actually popped (docs/control-api.md) — the text a
/// client splices back into its composer, with the id it was submitted under.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct WithdrawnTurn {
    pub submission_id: String,
    pub text: String,
}

#[derive(serde::Deserialize)]
struct WithdrawResponse {
    withdrawn: Vec<WithdrawnTurn>,
}

/// What a `POST /loop/continue` came back with (docs/control-api.md). A decline
/// is an outcome rather than an error: the caller renders the daemon's message
/// and leaves everything else as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinueOutcome {
    /// The loop accepted the continue and is taking another turn.
    Accepted,
    /// The moment had no continue in it — the loop is running or gated, or the
    /// wire ends where re-prompting it means nothing.
    Declined { message: String },
}

/// Pull one string field out of a JSON body — the `/auth/*` responses are all a
/// single-field object.
fn json_field(body: &str, field: &str) -> Result<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get(field).and_then(|f| f.as_str()).map(String::from))
        .ok_or_else(|| anyhow!("no {field:?} in response: {body}"))
}

impl Client {
    /// Join the base prefix and the API version onto a verb's path
    /// (docs/control-api.md): every session route is served under `/v1`, and
    /// prefixing here — the one place every request passes through — keeps the
    /// verbs above reading like the API they document.
    fn versioned(&self, path: &str) -> String {
        format!("{}/v1{path}", self.prefix)
    }
}

/// The JSON body for an approval decision — the four-valued [`Approval`] the loop consumes
/// (docs/control-channel.md). Shared by the blocking and non-blocking `approve` verbs.
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
