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

use crate::attachments::Arrival;
pub use crate::catalog::{CatalogModel, ConnectionCatalog, ModelRole, Status as CatalogStatus};
use crate::daemon::{
    ArchivedSession, CreateSession, PatchSession, SessionOptionsRequest, SessionState,
};
use crate::frontend::{Approval, SubmittedTurn};
use crate::inference::{ConnectionInventory, ResolvedOptions};

mod host;
mod transport;

pub use host::SessionRead;
pub use transport::Waited;
use transport::{json_field, read_2xx};
use trillium_client::Method;

/// A client bound to one daemon control socket. Cheap to [`Clone`] (the inner
/// trillium client is `Arc`-backed) so a client can hand a copy to a
/// background event-tail task.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    /// The path the session's `/v1` routes sit under: empty at a session bind's
    /// root, `/sessions/<id>` on a host (docs/control-api.md).
    prefix: String,
    /// The socket the client dials, when it dials one (not a test's in-memory
    /// connector): what a client names when the host stops answering.
    socket: Option<PathBuf>,
}

impl Client {
    /// Bind a client to the daemon's Unix socket. Dialing happens per request via
    /// the [`UnixClientConfig`] connector; nothing connects until a verb is called.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        let socket = socket.into();
        let mut client = Self::with_connector(UnixClientConfig::new(socket.clone()));
        client.socket = Some(socket);
        client
    }

    /// The socket this client dials, or `None` for a client built on a connector.
    pub fn socket(&self) -> Option<&std::path::Path> {
        self.socket.as_deref()
    }

    /// Bind a client to one session hosted behind a prefix — `/sessions/<id>` on
    /// a host's socket (docs/control-api.md). Every `/v1` verb joins onto the
    /// prefix; the `/auth/*` family does not, since credentials are the
    /// listener's, not a session's. The prefix is normalized to a leading slash
    /// and no trailing one, so `sessions/x/` and `/sessions/x` are the same base.
    pub fn at(socket: impl Into<PathBuf>, prefix: &str) -> Self {
        Self::new(socket).with_prefix(prefix)
    }

    /// The id of the session this client is bound to (`/sessions/<id>`), or
    /// `None` at a session bind's root.
    pub fn session_id(&self) -> Option<&str> {
        self.prefix.strip_prefix("/sessions/")
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
    /// — so this is the interface a test uses to drive the control router over
    /// `trillium_testing::connector(handler)`'s in-memory transport. [`HttpClient`]
    /// type-erases its connector, so this adds no type parameter to [`Client`] or to
    /// anything holding one.
    ///
    /// The same interface carries a non-UDS binding (TCP + TLS, docs/web-bind.md): the API
    /// surface is identical across bindings, and only the connector differs.
    pub fn with_connector(connector: impl Connector) -> Self {
        // The connector targets a fixed endpoint, so the URL host is irrelevant — but a
        // base is still needed to form request URLs. `localhost` is a placeholder that
        // only supplies the `Host` header (UnixClientConfig docs).
        let http = HttpClient::new(connector).with_base("http://localhost/");
        Self {
            http,
            prefix: String::new(),
            socket: None,
        }
    }

    /// Enqueue a user turn (`POST /turns`). Seeds turn 1 on an idle daemon. The
    /// daemon answers 202 immediately — submission is decoupled from inference, so
    /// the caller watches the stream (or `/log`) for the response.
    pub async fn turn(&self, text: &str) -> Result<()> {
        self.turn_with(text, TurnOptions::default()).await
    }

    /// [`turn`](Self::turn) with the body's optional fields ([`TurnOptions`]).
    pub async fn turn_with(&self, text: &str, options: TurnOptions<'_>) -> Result<()> {
        let body = Self::turn_body(text, None, options);
        self.post("/turns", body).await.map(drop)
    }

    /// Enqueue a turn the client built — its own submission id, its source, and any
    /// attachments it uploaded — naming the question it answers by `answers` (a handle
    /// or the ask's call id), or none (`POST /turns`, docs/control-api.md, docs/inbox.md).
    /// The id is recorded on the resulting `User` entry, so the caller can reconcile
    /// delivery by exact key rather than content. `Ok(Err)` is the host's refusal in its
    /// words: a question is pending and the turn names none, or the one it names is not
    /// pending.
    pub(crate) async fn submit(
        &self,
        turn: &SubmittedTurn,
        answers: Option<&str>,
    ) -> Result<std::result::Result<crate::daemon::Landing, String>> {
        let options = TurnOptions {
            source: turn.source.as_deref(),
        };
        let mut body: serde_json::Value = serde_json::from_str(&Self::turn_body(
            &turn.text,
            Some(&turn.submission_id),
            options,
        ))?;
        if !turn.attachments.is_empty() {
            body["attachments"] = turn
                .attachments
                .iter()
                .map(|arrival| serde_json::Value::from(arrival.name.as_str()))
                .collect();
        }
        if let Some(answers) = answers {
            body["answers"] = answers.into();
        }
        let path = self.versioned("/turns");
        match self
            .send_declinable(Method::Post, &path, Some(body.to_string()))
            .await?
        {
            Ok(answer) => {
                let delivery = json_field(&answer, "delivery").unwrap_or_default();
                Ok(Ok(crate::daemon::Landing::parse(&delivery)
                    .unwrap_or(crate::daemon::Landing::Queued)))
            }
            Err(declined) => Ok(Err(declined)),
        }
    }

    /// The `POST /turns` body: `text`, plus `submission_id` and `source`
    /// when present. Field order is fixed here so the body is the
    /// same bytes every call for the same inputs.
    fn turn_body(text: &str, submission_id: Option<&str>, options: TurnOptions<'_>) -> String {
        let mut body = serde_json::json!({ "text": text });
        if let Some(submission_id) = submission_id {
            body["submission_id"] = serde_json::Value::from(submission_id);
        }
        if let Some(source) = options.source {
            body["source"] = serde_json::Value::from(source);
        }
        body.to_string()
    }

    /// Upload a file for a turn to carry (`POST /attachments`, docs/inbox.md),
    /// returning what the daemon stored: the name to send it under, the path the
    /// model will read, and its triage signals.
    pub async fn upload_attachment(&self, name: &str, bytes: Vec<u8>) -> Result<Arrival> {
        let query: String = trillium_client::url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", name)
            .finish();
        let path = self.versioned(&format!("/attachments?{query}"));
        let path = path.as_str();
        let conn = self
            .http
            .post(path)
            .with_request_header("content-type", "application/octet-stream")
            .with_body(bytes)
            .await
            .with_context(|| format!("POST {path} to the control socket"))?;
        let body = read_2xx(conn, Method::Post, path).await?;
        serde_json::from_str(&body).with_context(|| format!("parsing the upload response: {body}"))
    }

    /// Remove a staged upload the operator took back before sending
    /// (`DELETE /attachments/:name`).
    pub async fn remove_attachment(&self, name: &str) -> Result<()> {
        let name: String =
            percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
                .collect();
        self.delete(&format!("/attachments/{name}")).await.map(drop)
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

    /// Resolve the pending approval gate (`PUT /approvals/{call_id}`), carrying the
    /// same four-valued [`Approval`] the loop consumes (docs/control-channel.md).
    /// The id is a precondition: addressing anything but the pending gate answers
    /// 409 `gate_moved`, with the current gate in the error's `detail`. `source` names
    /// the decider as a turn's `source` would; `None` is the operator. The host records
    /// both (docs/control-api.md).
    pub async fn approve(
        &self,
        call_id: &str,
        decision: &Approval,
        source: Option<&str>,
    ) -> Result<()> {
        self.put(
            &format!("/approvals/{call_id}"),
            approval_body(decision, source, None),
        )
        .await
        .map(drop)
    }

    /// [`approve`](Self::approve), with a moved gate as an answer rather than an error:
    /// `Ok(false)` when `call_id` is no longer the pending gate (`409 gate_moved`).
    pub(crate) async fn decide(
        &self,
        call_id: &str,
        decision: &Approval,
        source: Option<&str>,
        note_submission: Option<&str>,
    ) -> Result<bool> {
        let path = self.versioned(&format!("/approvals/{call_id}"));
        let body = approval_body(decision, source, note_submission);
        let answer = self.send_declinable(Method::Put, &path, Some(body)).await?;
        Ok(answer.is_ok())
    }

    /// Escalate the pending approval gate to the operator
    /// (`POST /approvals/{call_id}/escalate`, docs/control-api.md): it stays held, marked
    /// with `source` as its escalator and `note` for the operator. `Ok(false)` when
    /// `call_id` is no longer the pending gate (`409 gate_moved`).
    pub(crate) async fn escalate(
        &self,
        call_id: &str,
        source: Option<&str>,
        note: Option<&str>,
    ) -> Result<bool> {
        let path = self.versioned(&format!("/approvals/{call_id}/escalate"));
        let body = serde_json::json!({ "source": source, "note": note }).to_string();
        let answer = self
            .send_declinable(Method::Post, &path, Some(body))
            .await?;
        Ok(answer.is_ok())
    }

    /// Block until the daemon next reaches a decision point — the loop phase is idle with
    /// nothing queued, gated, asking, cold-parked, or blocked — and return the `/state`
    /// snapshot (`GET /state?wait=settled`), whose `loop_phase` names which. Returns
    /// immediately if already settled (docs/control-api.md).
    pub async fn settle(&self) -> Result<Waited> {
        self.wait_at(Method::Get, "/state?wait=settled", None).await
    }

    /// Set the session's title (`PUT /title`, docs/session-title.md). Answers
    /// with the `session_identity` topic as it now reads.
    pub async fn set_title(&self, title: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "title": title }).to_string();
        let resp = self.put("/title", body).await?;
        serde_json::from_str(&resp).map_err(|_| anyhow!("bad /title response: {resp}"))
    }

    /// Interrupt the loop (`POST /loop/interrupt`, docs/control-api.md) — the SIGINT
    /// equivalent that kills a running tool's process group (docs/process-monitor.md) —
    /// saying who is asking and through which affordance; the host records both.
    pub async fn interrupt(&self, origin: &crate::log::InterruptOrigin) -> Result<()> {
        let body = serde_json::to_string(origin)?;
        self.post("/loop/interrupt", body).await.map(drop)
    }

    /// Compact a cold session's history into a summary (`POST /loop/compact`,
    /// docs/control-api.md). Accepted means the session's model is writing it; the
    /// summary lands on the log when it is done. A decline (`409 not_compactable`) is a
    /// normal answer whose message says why.
    pub async fn compact(&self) -> Result<CompactOutcome> {
        Ok(
            match self
                .post_declinable("/loop/compact", "compaction", None)
                .await?
            {
                None => CompactOutcome::Accepted,
                Some(message) => CompactOutcome::Declined { message },
            },
        )
    }

    /// Hand control back with no input (`POST /loop/continue`, docs/control-api.md):
    /// the idle loop takes another turn on the wire it already has. Nothing is
    /// submitted and nothing is appended, so this is not a turn with empty text —
    /// that one keeps the loop idling.
    ///
    /// A decline (`409 not_continuable`) is a normal answer, not a transport
    /// failure: the moment simply had no continue in it, and the daemon's message
    /// says which case. Errors are reserved for a request that didn't get an answer.
    pub async fn continue_loop(&self, source: Option<&str>) -> Result<ContinueOutcome> {
        let origin = crate::log::ContinueOrigin {
            source: source.map(str::to_string),
        };
        let body = serde_json::to_string(&origin)?;
        Ok(
            match self
                .post_declinable("/loop/continue", "continue", Some(body))
                .await?
            {
                None => ContinueOutcome::Accepted,
                Some(message) => ContinueOutcome::Declined { message },
            },
        )
    }

    /// POST to a loop verb whose `409` is an answer rather than a failure: `None`
    /// when accepted, the daemon's message when declined.
    async fn post_declinable(
        &self,
        path: &str,
        verb: &str,
        body: Option<String>,
    ) -> Result<Option<String>> {
        let path = self.versioned(path);
        let mut conn = self.exchange(Method::Post, &path, body).await?;
        if conn.status() != Some(trillium_client::Status::Conflict) {
            return read_2xx(conn, Method::Post, &path).await.map(|_| None);
        }
        let body = conn
            .response_body()
            .read_string()
            .await
            .with_context(|| format!("reading the response to POST {path}"))?;
        Ok(Some(json_field(&body, "message").unwrap_or_else(|_| {
            format!("the daemon declined the {verb}")
        })))
    }

    /// The running processes (`GET /processes`, docs/process-monitor.md) — foreground and
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
    /// (`PATCH /processes/:id {background: true}`, docs/process-monitor.md) — keep it
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

    /// Kill a background process (`DELETE /processes/:id`, docs/process-monitor.md) —
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
    /// docs/permissions.md): `autonomous` / `supervisor` / `spotter_human` / `human_only`.
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

    /// File an ambient notification in the daemon's inbox (`POST /notifications`,
    /// docs/inbox.md). `slot` binds it to a logical channel so a later notify on the same
    /// slot supersedes it. Returns the arrival entry's `seq`, or `None` for a slot
    /// notification, which has no entry until a triage shows it.
    pub async fn notify(
        &self,
        source: &str,
        body: &str,
        slot: Option<&str>,
    ) -> Result<Option<u64>> {
        let mut payload = serde_json::json!({ "source": source, "body": body });
        if let Some(s) = slot {
            payload["slot"] = serde_json::json!(s);
        }
        let resp = self.post("/notifications", payload.to_string()).await?;
        let value = serde_json::from_str::<serde_json::Value>(&resp)
            .map_err(|e| anyhow!("bad /notifications response {resp}: {e}"))?;
        Ok(value.get("seq").and_then(|i| i.as_u64()))
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

    /// The transcript (`GET /transcript`, docs/control-api.md): a log window as plain
    /// text, at the grain and for the reader `read` names. `since` resumes forward,
    /// `tail` takes the last N; both together is the daemon's `400`.
    pub async fn transcript(&self, read: TranscriptRead<'_>) -> Result<String> {
        let query = read.query();
        let path = if query.is_empty() {
            "/transcript".to_string()
        } else {
            format!("/transcript?{query}")
        };
        self.get(&path).await
    }

    /// One whole entry by `seq` (`GET /log/:seq`, docs/control-api.md): the full
    /// entry view, an oversized result's content included.
    pub async fn log_entry(&self, seq: u64) -> Result<String> {
        self.get(&format!("/log/{seq}")).await
    }
    /// Resolve a closed `html-artifact` fence from a committed assistant text block
    /// (`GET /artifacts/:seq/:text_block/:fence`). Coordinates are zero-based. The
    /// response's `external_url` is the anonymous artifact-only loopback address for
    /// clients such as the TUI; `url` is the session-relative artifact address.
    pub async fn resolve_artifact(
        &self,
        seq: u64,
        text_block: usize,
        fence: usize,
    ) -> Result<ArtifactResolution> {
        let path = format!("/artifacts/{seq}/{text_block}/{fence}");
        let body = self.get(&path).await?;
        serde_json::from_str(&body)
            .with_context(|| format!("parsing the artifact resolution response: {body}"))
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

    /// One logged result image's decoded bytes (`GET /log/:seq/images/:idx`,
    /// docs/control-api.md) — the dereference target for the image references
    /// entry views carry (`data` is omitted there; this is where it lives).
    pub async fn log_image(&self, seq: u64, idx: usize) -> Result<Vec<u8>> {
        self.get_bytes(&format!("/log/{seq}/images/{idx}")).await
    }
}

/// One entry of `GET /sessions` (docs/control-api.md). The host defines the row
/// and both readings of the resource serve it, so there is one type on the wire
/// rather than a client copy that can drift.
pub use crate::daemon::ListedSession;

/// One turn a `DELETE /turns/:submission_id` actually popped (docs/control-api.md) — the text a
/// client splices back into its composer, with the id it was submitted under, and the uploads it
/// carried, still in the store to be staged again.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct WithdrawnTurn {
    pub submission_id: String,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<Arrival>,
}
/// The artifact route's resolver response: a relative token URL and, when the
/// artifact-only listener is available, its loopback URL for direct display/opening.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct ArtifactResolution {
    pub url: String,
    #[serde(default)]
    pub external_url: Option<String>,
}

#[derive(serde::Deserialize)]
struct WithdrawResponse {
    withdrawn: Vec<WithdrawnTurn>,
}

/// What a `POST /loop/compact` came back with (docs/control-api.md). A decline is a
/// normal answer: the moment had no compaction in it, and the message says which case.
#[derive(Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    /// The session's model is writing the summary.
    Accepted,
    Declined {
        message: String,
    },
}

/// What a `POST /loop/continue` came back with (docs/control-api.md). A decline
/// is an outcome rather than an error: the caller renders the daemon's message
/// and leaves everything else as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinueOutcome {
    /// The loop accepted the continue and is taking another turn.
    Accepted,
    /// The moment had no continue in it — the loop is busy or gated, or the
    /// wire ends where re-prompting it means nothing.
    Declined { message: String },
}

/// The options a `POST /turns` body carries beside its text (docs/control-api.md).
#[derive(Clone, Copy, Debug, Default)]
pub struct TurnOptions<'a> {
    /// Who is submitting, when it is not the operator: the client's self-chosen
    /// label (`web`, `mcp[<name>]`, `session[<id>]`). `None` submits as the
    /// operator, who renders no prefix.
    pub source: Option<&'a str>,
}

/// Which `GET /transcript` read to make (docs/control-api.md).
#[derive(Clone, Copy, Debug, Default)]
pub struct TranscriptRead<'a> {
    /// Only entries with `seq >= since`.
    pub since: Option<u64>,
    /// Only entries with `seq < before`.
    pub before: Option<u64>,
    /// Only the last `tail` entries.
    pub tail: Option<u64>,
    /// One entry in full by its handle (`8`, `8.2`), in place of a window.
    pub seq: Option<&'a str>,
    /// The entry-by-entry audit render (`grain=audit`) instead of the conversation.
    pub audit: bool,
    /// The reader, as a `source` (`mcp[<name>]`, `session[<id>]`) or a session id: a
    /// sender or decider by that name reads "you".
    pub me: Option<&'a str>,
}

impl TranscriptRead<'_> {
    /// The read as a `GET /transcript` query string, percent-encoded.
    fn query(&self) -> String {
        let mut query = trillium_client::url::form_urlencoded::Serializer::new(String::new());
        if let Some(since) = self.since {
            query.append_pair("since", &since.to_string());
        }
        if let Some(before) = self.before {
            query.append_pair("before", &before.to_string());
        }
        if let Some(seq) = self.seq {
            query.append_pair("seq", seq);
        }
        if let Some(tail) = self.tail {
            query.append_pair("tail", &tail.to_string());
        }
        if self.audit {
            query.append_pair("grain", "audit");
        }
        if let Some(me) = self.me {
            query.append_pair("me", me);
        }
        query.finish()
    }
}

/// The JSON body for an approval decision — the four-valued [`Approval`] the loop consumes
/// (docs/control-channel.md). Shared by `approve` and `decide`; `note_submission` names
/// the turn that carries an allow's note.
fn approval_body(
    decision: &Approval,
    source: Option<&str>,
    note_submission: Option<&str>,
) -> String {
    let mut body = match decision {
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
    };
    if let Some(source) = source {
        body["source"] = serde_json::json!(source);
    }
    if let Some(submission_id) = note_submission {
        body["note_submission"] = serde_json::json!(submission_id);
    }
    body.to_string()
}

#[cfg(test)]
mod test;
