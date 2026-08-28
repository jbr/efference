//! The HTTP surface: request/response bodies, the router, and its query helpers (docs/control-channel.md).

use super::*;

#[derive(Deserialize)]
pub(crate) struct TurnBody {
    pub(crate) text: String,
    /// The client's identifier for this submission (docs/control-api.md),
    /// adopted when present and daemon-minted otherwise — the conn-id
    /// adopt-or-mint rule at submission granularity. Echoed in the 202 body
    /// and recorded on the resulting `User` entry as `submission_id`.
    #[serde(default)]
    pub(crate) submission_id: Option<String>,
}

/// Cap on a client-minted submission id: long enough for any uuid-ish scheme,
/// short enough that the log can't be bloated through the field.
const SUBMISSION_ID_MAX: usize = 128;

/// Mint a submission id for a `POST /v1/turns` whose body didn't carry one, so
/// every submitted turn is identified in the log regardless of client
/// (docs/control-api.md). Public because a client that wants its submission
/// addressable — for the delivery echo, or to recall it — mints its own with
/// the same scheme rather than inventing a second one (docs/tui.md).
pub fn mint_submission_id() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Body of `PATCH /processes/:id` (docs/supervisor.md, docs/control-api.md): the
/// settable properties of a process resource. `background: true` on the foreground
/// entry is the foreground→background handoff; `note` is the operator note the
/// handoff delivers as the next user message — a turn-injection concern riding
/// beside the property change, never a process attribute.
#[derive(Deserialize)]
pub(crate) struct ProcessPatchBody {
    #[serde(default)]
    pub(crate) background: Option<bool>,
    #[serde(default)]
    pub(crate) muted: Option<bool>,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

/// Body of `PATCH /proposals/:seq`: the orchestrator moves a pending proposal to its
/// terminal state (`committed` | `cancelled`), with an optional rationale note
/// (docs/permissions.md).
#[derive(Deserialize)]
pub(crate) struct ProposalPatchBody {
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DecisionBody {
    pub(crate) decision: String,
    #[serde(default)]
    pub(crate) message: Option<String>,
    /// The config-edit payload (`decision: "config_edit"` only) — the change and a rationale note
    /// (docs/permissions.md). A committed change is always written to the session's
    /// config file, so there is no lifetime to choose; a `lifetime` field from an older client is
    /// ignored rather than rejected.
    #[serde(default)]
    pub(crate) change: Option<crate::log::ConfigChange>,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

/// Body of `PATCH /loop` (docs/control-channel.md): the operator dials, each
/// optional so one call can move either or both. `pace` is itself partial —
/// `{"pace":{"speed_ms":10000}}` slows, `{"pace":{"held":true}}` pauses,
/// `{"pace":{"held":false}}` resumes; `review_level` is one of `autonomous` /
/// `spotter_human` / `human_only` (docs/permissions.md).
#[derive(Deserialize)]
pub(crate) struct LoopPatchBody {
    #[serde(default)]
    pub(crate) pace: Option<PaceBody>,
    #[serde(default)]
    pub(crate) review_level: Option<String>,
}

/// The `pace` half of [`LoopPatchBody`].
#[derive(Deserialize)]
pub(crate) struct PaceBody {
    #[serde(default)]
    pub(crate) speed_ms: Option<u64>,
    #[serde(default)]
    pub(crate) held: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct NotifyBody {
    pub(crate) source: String,
    pub(crate) body: String,
    /// Logical channel — a later notify on the same slot supersedes this one (docs/inbox.md).
    #[serde(default)]
    pub(crate) slot: Option<String>,
    /// Supersede a specific pending notification by its queue-local id.
    #[serde(default)]
    pub(crate) replaces: Option<u64>,
}

/// Pull the shared daemon state out of a conn's server-global TypeSet, or short
/// out with a 500 if it's somehow absent (a wiring bug, never seen at runtime).
pub(crate) fn shared_of(conn: &Conn) -> Option<Arc<Shared>> {
    conn.shared_state::<Arc<Shared>>().map(Arc::clone)
}

/// The answer when the server-global state is absent — the one condition every route
/// shares, and the reason it has a helper rather than a body constant.
fn missing_state(conn: Conn) -> Conn {
    fail(conn, ErrorCode::Internal, "missing shared state")
}

/// Mark a response as never changing. Valid only for reads of a single log entry or the
/// call inside one: a `LogEntry` is written once and never mutated — redaction chooses
/// `wire` at append time from the verdict rather than flipping it afterward
/// (docs/log-and-projection.md) — so a client that has seen one need never refetch it.
/// Not valid for `GET /log`, whose window's *contents* are immutable but whose answer to
/// any given query grows with the session.
fn immutable(conn: Conn) -> Conn {
    conn.with_response_header("cache-control", "public, max-age=31536000, immutable")
}

/// How long `/events` may sit silent before it sends an SSE comment (docs/control-api.md).
///
/// An idle stream through a TLS intermediary gets reaped, and a client cannot otherwise
/// distinguish idle from dead; a comment (`: `) is the one keepalive an `EventSource`
/// consumer ignores — every shape `Eventable` can encode fires `onmessage` — and the one
/// that stays out of the frame envelope, since a heartbeat *frame* would need a fourth
/// `class` naming no durability. It is also what makes `?since` past the head safe to
/// serve as a wait rather than an error. (`daemon/keepalive.rs` is the *upstream* cache
/// ping; unrelated.)
const EVENTS_HEARTBEAT: Duration = Duration::from_secs(15);

/// The HTTP surface — a `trillium-router` over the verb set (docs/control-channel.md). Each
/// handler reads `Shared` from the server-global state; JSON in, JSON out. The
/// router gives method-aware dispatch (and a clean `:call_id` capture) in place
/// of the earlier hand-rolled `(method, path)` match.
///
/// `heartbeat` is the `/events` idle-comment interval — [`EVENTS_HEARTBEAT`] via
/// [`router`] everywhere real; a parameter so a test can shrink it and watch a quiet
/// stream stay audibly alive.
pub(crate) fn router_with_heartbeat(heartbeat: Duration) -> Router {
    Router::new()
        // Enqueue a user turn (also seeds turn 1). 202 — watch the stream for the
        // response; submission is decoupled from inference.
        .post("/v1/turns", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let wait = wants_wait(conn.querystring());
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<TurnBody>(&body) {
                Ok(TurnBody {
                    text,
                    submission_id,
                }) => {
                    if submission_id
                        .as_ref()
                        .is_some_and(|id| id.len() > SUBMISSION_ID_MAX || id.is_empty())
                    {
                        return fail(
                            conn,
                            ErrorCode::BadRequest,
                            format!("submission_id must be 1–{SUBMISSION_ID_MAX} bytes"),
                        );
                    }
                    let submission_id = submission_id.unwrap_or_else(mint_submission_id);
                    shared.submit_turn(SubmittedTurn {
                        text,
                        submission_id: submission_id.clone(),
                    });
                    // `?wait`: hold the connection until the turn reaches a gate or completes,
                    // then answer with the resulting state — "act → read state" in one round-trip
                    // (docs/permissions.md).
                    if wait {
                        settle_and_state(&shared, conn).await
                    } else {
                        // The id is informational here: the turn's effects — including
                        // the `User` entry carrying this id — can arrive on the stream
                        // before this body resolves, so clients must not sequence
                        // state on it (docs/control-api.md).
                        let body = serde_json::json!({
                            "status": "accepted",
                            "submission_id": submission_id,
                        });
                        conn.with_status(202).with_body(body.to_string()).halt()
                    }
                }
                Err(e) => fail(conn, ErrorCode::BadRequest, format!("bad /turns body: {e}")),
            }
        })
        // Withdraw the pending-queue suffix beginning at this submission id — recall
        // (docs/control-api.md). Always 200: the loop may have dequeued the turn already,
        // and "popped nothing" is a race outcome the client renders, not an error. The
        // response is authoritative — a client splices only what came back.
        .delete("/v1/turns/:submission_id", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(id) = conn.param("submission_id").map(str::to_string) else {
                return fail(conn, ErrorCode::BadRequest, "missing submission id");
            };
            let withdrawn: Vec<Value> = shared
                .withdraw_turns(&id)
                .into_iter()
                .map(|t| serde_json::json!({ "submission_id": t.submission_id, "text": t.text }))
                .collect();
            let body = serde_json::json!({ "withdrawn": withdrawn });
            conn.with_status(200).with_body(body.to_string()).halt()
        })
        // Resolve the pending approval gate, addressed by the call awaiting it. A
        // `PUT` with the id as a real precondition: the decision applies only to
        // the gate the client is looking at.
        .put("/v1/approvals/:call_id", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let id = CallId(conn.param("call_id").unwrap_or_default().to_string());
            let wait = wants_wait(conn.querystring());
            let body = conn.request_body_string().await.unwrap_or_default();
            let decision = match serde_json::from_str::<DecisionBody>(&body) {
                Ok(d) => d,
                Err(e) => {
                    return fail(
                        conn,
                        ErrorCode::BadRequest,
                        format!("bad decision body: {e}"),
                    );
                }
            };
            let approval = match decision.decision.as_str() {
                "approve" => Approval::Approve,
                "abort" => Approval::Abort,
                "redirect" => Approval::Redirect(decision.message.unwrap_or_default()),
                "config_edit" => match decision.change {
                    Some(change) => Approval::ConfigEdit {
                        change,
                        note: decision.note.unwrap_or_default(),
                    },
                    None => {
                        return fail(
                            conn,
                            ErrorCode::BadRequest,
                            "config_edit decision requires a change",
                        );
                    }
                },
                // Well-formed JSON naming a decision this daemon doesn't implement: the
                // client is version-skewed rather than buggy, which is the distinction
                // `InvalidDecision` exists to carry (errors.rs).
                other => {
                    return fail(
                        conn,
                        ErrorCode::InvalidDecision,
                        format!("unknown decision: {other}"),
                    );
                }
            };
            if shared.resolve_approval(&id, approval) {
                // `?wait`: hold until the turn reaches its *next* gate or completes — so a
                // multi-gate turn is driven as a clean "approve → read state" loop.
                if wait {
                    settle_and_state(&shared, conn).await
                } else {
                    conn.with_status(200)
                        .with_body(r#"{"status":"resolved"}"#)
                        .halt()
                }
            } else {
                // The precondition failed: that id is not the pending gate — already
                // resolved, or the loop moved on. 409 with the *current* gate as
                // corrective detail (`null` = none pending), so a client re-renders
                // without a second round-trip. This closes the "approved a gate that
                // already moved on" race the old ambiguous 404 left open.
                respond(
                    conn,
                    ErrorBody::new(
                        ErrorCode::GateMoved,
                        "that call is not the pending approval gate",
                    )
                    .with_detail(shared.topic_json(Topic::Gate)),
                )
            }
        })
        // Signal the running tool (docs/control-channel.md). A verb-route on the
        // loop resource because it *is* a verb — it signals, it doesn't delete
        // state.
        .post("/v1/loop/interrupt", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            shared.interrupt();
            conn.with_status(200)
                .with_body(r#"{"status":"interrupted"}"#)
                .halt()
        })
        // Hand control back with no input (docs/control-api.md): the idle loop takes
        // another turn on the wire it already has — nothing is appended, nothing is
        // projected differently. A verb-route on the loop resource beside
        // `/loop/interrupt`, for the same reason: it signals the loop rather than
        // creating or editing a resource. Declined with `409 not_continuable` when
        // the moment has no continue in it.
        .post("/v1/loop/continue", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            match shared.request_continue() {
                Ok(()) => conn
                    .with_status(202)
                    .with_body(r#"{"status":"continuing"}"#)
                    .halt(),
                Err(why) => fail(conn, ErrorCode::NotContinuable, why.message()),
            }
        })
        // The turn engine's operator dials as one resource (docs/control-channel.md):
        // `pace`, `review_level`, and the read-only `running` — named exactly as their
        // state topics, since they are that resource's fields.
        .get("/v1/loop", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            conn.with_status(200).with_body(shared.loop_json()).halt()
        })
        // Move the dials — either or both in one call, which the two verb-routes
        // this replaces could not do. Validated whole before anything is applied,
        // so a request that is part-bad moves nothing.
        .patch("/v1/loop", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            let patch = match serde_json::from_str::<LoopPatchBody>(&body) {
                Ok(p) => p,
                Err(e) => {
                    return fail(conn, ErrorCode::BadRequest, format!("bad /loop body: {e}"));
                }
            };
            let level = match patch.review_level.as_deref() {
                Some(level) => match crate::policy::ReviewLevel::parse(level) {
                    Some(level) => Some(level),
                    // An out-of-range value on a well-formed body — `BadRequest`
                    // rather than `InvalidDecision`, which is reserved for a
                    // *decision* verb the daemon doesn't implement (errors.rs).
                    None => {
                        return fail(
                            conn,
                            ErrorCode::BadRequest,
                            "review_level must be autonomous | spotter_human | human_only",
                        );
                    }
                },
                None => None,
            };
            if let Some(pace) = patch.pace {
                shared.set_pace(pace.speed_ms.map(Duration::from_millis), pace.held);
            }
            if let Some(level) = level {
                shared.set_review_level(level);
            }
            conn.with_status(200).with_body(shared.loop_json()).halt()
        })
        // The process resource (docs/supervisor.md, docs/control-api.md): every
        // running tool-spawned process — foreground and background share the one
        // namespace, `:id` is the bare spawn-minted ordinal. The listing is the
        // same representation the `processes` state topic carries.
        .get("/v1/processes", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let body =
                serde_json::to_string(&shared.process_states()).unwrap_or_else(|_| "[]".into());
            conn.with_status(200).with_body(body).halt()
        })
        // One running process's representation.
        .get("/v1/processes/:id", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
            };
            match shared
                .running_process(id)
                .map(|p| serde_json::to_string(&p))
            {
                Some(Ok(body)) => conn.with_status(200).with_body(body).halt(),
                _ => fail(conn, ErrorCode::NotFound, "no running process with that id"),
            }
        })
        // A cursor read of one process's retained output. `?since=<cursor>` resumes
        // after a previous read; with no cursor it returns everything retained — the
        // seed a cold-opened client renders before its first `process_output` delta
        // (docs/control-api.md). Works on exited processes too: the capture outlives
        // the exit, unlike the listing entry.
        .get("/v1/processes/:id/output", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
            };
            let since = match parse_u64_param(conn.querystring(), "since") {
                Ok(since) => since,
                Err(message) => return fail(conn, ErrorCode::BadRequest, message),
            };
            match shared.process_output_read(id, since) {
                Some(read) => {
                    let body = serde_json::json!({
                        "id": id,
                        "output": read.content,
                        "cursor": read.cursor,
                        "lost": read.lost,
                        "running": read.running,
                    });
                    conn.with_status(200).with_body(body.to_string()).halt()
                }
                None => fail(conn, ErrorCode::NotFound, "no process with that id"),
            }
        })
        // Set a process's properties. `{"muted": …}` applies synchronously (the
        // supervisor's dismiss-a-live-process, and its inverse). `{"background":
        // true}` on the foreground entry is the foreground→background handoff
        // (docs/supervisor.md): answered 202, because the flip lands only when the
        // detach wake reaches the running tool's race — on a process that is
        // already background it is an idempotent no-op. An optional `note` rides
        // the handoff as the next user message. `background: false` has no
        // transition to name.
        .patch("/v1/processes/:id", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            let patch = match serde_json::from_str::<ProcessPatchBody>(&body) {
                Ok(p) => p,
                Err(e) => {
                    return fail(
                        conn,
                        ErrorCode::BadRequest,
                        format!("bad /processes body: {e}"),
                    );
                }
            };
            if patch.background == Some(false) {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "a background process cannot be foregrounded",
                );
            }
            if patch.note.is_some() && patch.background != Some(true) {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "note accompanies a background:true handoff",
                );
            }
            let Some(current) = shared.running_process(id) else {
                return fail(conn, ErrorCode::NotFound, "no running process with that id");
            };
            if let Some(muted) = patch.muted {
                shared.set_process_muted(id, muted);
            }
            // Only a foreground entry has anywhere to move; the id addressing what
            // to detach is what the blind verb-route this replaced couldn't say.
            let detach = patch.background == Some(true) && !current.background;
            if detach {
                shared.request_detach(patch.note);
            }
            let representation = shared
                .running_process(id)
                .and_then(|p| serde_json::to_string(&p).ok())
                .unwrap_or_else(|| "null".into());
            conn.with_status(if detach { 202 } else { 200 })
                .with_body(representation)
                .halt()
        })
        // Kill a running process: `killpg` its group; the drain task then sees EOF
        // and fires the exit notification (docs/supervisor.md).
        .delete("/v1/processes/:id", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
            };
            if shared.kill_process(id) {
                conn.with_status(200)
                    .with_body(r#"{"status":"killed"}"#)
                    .halt()
            } else {
                fail(conn, ErrorCode::NotFound, "no running process with that id")
            }
        })
        // Enqueue an ambient notification into the inbox (docs/inbox.md). 202 — it waits
        // for the next triage gate; a client watches the `notifications` topic.
        .post("/v1/notifications", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<NotifyBody>(&body) {
                Ok(n) => {
                    let id = shared.notify(n.source, n.body, n.slot, n.replaces);
                    conn.with_status(202)
                        .with_body(format!(r#"{{"id":{id}}}"#))
                        .halt()
                }
                Err(e) => fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!("bad /notifications body: {e}"),
                ),
            }
        })
        // Dismiss a pending notification out-of-band by its queue-local id (docs/inbox.md).
        .delete("/v1/notifications/:id", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            match conn.param("id").and_then(|s| s.parse::<u64>().ok()) {
                Some(id) if shared.dismiss_notification(id) => conn
                    .with_status(200)
                    .with_body(r#"{"status":"dismissed"}"#)
                    .halt(),
                Some(_) => fail(
                    conn,
                    ErrorCode::NotFound,
                    "no pending notification with that id",
                ),
                None => fail(
                    conn,
                    ErrorCode::BadRequest,
                    "notification id must be an integer",
                ),
            }
        })
        // List async config-edit proposals awaiting an orchestrator decision
        // (docs/permissions.md). The same array `/state` carries; a
        // dedicated GET lets a client poll just this without the rest of the snapshot.
        .get("/v1/proposals", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let body =
                serde_json::to_string(&shared.pending_proposals()).unwrap_or_else(|_| "[]".into());
            conn.with_status(200).with_body(body).halt()
        })
        // Move a proposal to its terminal state by its `seq` (the `Proposed` entry's
        // id). Body: `{"state":"committed"|"cancelled","note":"…"}`. Committing folds
        // the proposed change into the effective config (and notifies the session for
        // scope/paths); cancelling is audit-only. Either way the proposal leaves the
        // pending queue. The orchestrator-above legislates here — the session can only
        // *file* a proposal, never resolve its own.
        .patch("/v1/proposals/:seq", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "proposal seq must be an integer",
                );
            };
            let raw = conn.request_body_string().await.unwrap_or_default();
            let decision = match serde_json::from_str::<ProposalPatchBody>(&raw) {
                Ok(d) => d,
                Err(e) => {
                    return fail(
                        conn,
                        ErrorCode::BadRequest,
                        format!("bad /proposals body: {e}"),
                    );
                }
            };
            let commit = match decision.state.as_str() {
                "committed" => true,
                "cancelled" => false,
                other => {
                    return fail(
                        conn,
                        ErrorCode::InvalidDecision,
                        format!("state must be \"committed\" or \"cancelled\", got {other:?}"),
                    );
                }
            };
            match shared.resolve_proposal_decision(seq, commit, decision.note.unwrap_or_default()) {
                Ok(true) => conn
                    .with_status(200)
                    .with_body(if commit {
                        r#"{"state":"committed"}"#
                    } else {
                        r#"{"state":"cancelled"}"#
                    })
                    .halt(),
                Ok(false) => fail(
                    conn,
                    ErrorCode::NotFound,
                    "no pending proposal with that seq",
                ),
                Err(e) => fail(conn, ErrorCode::Internal, format!("append failed: {e}")),
            }
        })
        // The composite current representation — every state topic under its own
        // name (docs/control-api.md). `?wait=settled` blocks until the daemon
        // reaches a decision point — a gate is pending, or the loop is idle — then
        // answers with the same snapshot: the wait-without-acting read that
        // replaces polling in a wall-clock loop (docs/permissions.md). Returns at
        // once if already settled.
        .get("/v1/state", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            match parse_param(conn.querystring(), "wait") {
                Some("settled") => settle_and_state(&shared, conn).await,
                // An unknown predicate is an error, not an immediate answer: a
                // client that asked to wait must not be told "now" is what it
                // waited for.
                Some(other) => {
                    let message = format!("unknown wait predicate {other:?}; try wait=settled");
                    fail(conn, ErrorCode::BadRequest, message)
                }
                None if conn.querystring().split('&').any(|kv| kv == "wait") => fail(
                    conn,
                    ErrorCode::BadRequest,
                    "wait needs a predicate: wait=settled",
                ),
                None => conn.with_status(200).with_body(shared.state_json()).halt(),
            }
        })
        // The durable history as a JSON array. Unbounded by default; `?since=<seq>`
        // resumes forward, `?tail=N` opens cold at the tail, `?before=<seq>&tail=N` pages
        // one window backward (docs/control-api.md). A window may be cut anywhere —
        // references that dangle across the cut resolve through the two routes below.
        .get("/v1/log", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            match LogSelect::parse(conn.querystring()) {
                Ok(select) => conn
                    .with_status(200)
                    .with_body(shared.log_json(&select))
                    .halt(),
                Err(message) => fail(conn, ErrorCode::BadRequest, message),
            }
        })
        // One entry by `seq` — the general form of reference resolution for a client
        // reading a window rather than the whole log (docs/control-api.md).
        .get("/v1/log/:seq", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(conn, ErrorCode::BadRequest, "log seq must be an integer");
            };
            match shared.entry_json(seq) {
                Some(body) => immutable(conn).with_status(200).with_body(body).halt(),
                None => fail(conn, ErrorCode::NotFound, "no log entry with that seq"),
            }
        })
        // One logged result image's bytes (docs/control-api.md): the dereference target
        // for the image references every entry view carries — views omit an image's
        // base64 `data` (entry_view.rs), and a client fetches the pixels here instead,
        // where they are HTTP-cached rather than re-shipped with every window or
        // replay. Raw bytes under the stored media type, so the URL works directly as
        // an `<img src>`; immutable like any single-entry read.
        .get("/v1/log/:seq/images/:idx", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
                return fail(conn, ErrorCode::BadRequest, "log seq must be an integer");
            };
            let Some(idx) = conn.param("idx").and_then(|s| s.parse::<usize>().ok()) else {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "image index must be an integer",
                );
            };
            match shared.image_bytes(seq, idx) {
                Some((media_type, bytes)) => immutable(conn)
                    .with_response_header("content-type", media_type)
                    .with_status(200)
                    .with_body(bytes)
                    .halt(),
                None => fail(conn, ErrorCode::NotFound, "no image at that seq and index"),
            }
        })
        // One tool call by the `call_id` the log, the gate, and `tool_output` frames all
        // already carry. A call is embedded in an `Assistant` entry rather than being one,
        // which is why it needs an address of its own (docs/control-api.md).
        .get("/v1/tool_calls/:call_id", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return missing_state(conn);
            };
            let call_id = conn.param("call_id").unwrap_or_default().to_string();
            match shared.tool_call_json(&call_id) {
                Some(body) => immutable(conn).with_status(200).with_body(body).halt(),
                None => fail(conn, ErrorCode::NotFound, "no tool call with that id"),
            }
        })
        // Live tail of the append-only log (docs/README.md — the log *is* the event
        // store). `?since=<seq>` replays from there then follows; default 0 streams
        // the whole session and then live. Each event carries `id: <seq>`, so a
        // reconnecting client resumes with `?since=<last_seq + 1>` — the explicit
        // cursor the SSE crate's bare framing leaves to us.
        .get(
            "/v1/events",
            (
                // The route is a tuple because the `Sse` handler cannot answer with the
                // typed envelope: its `connect` must return a stream, so rejection
                // happens *before* it, and a failure halts so the SSE half never runs.
                // The `Sse` handler brings what the plain `with_sse_stream` conn method
                // cannot: `Accept` negotiation (a request excluding `text/event-stream`
                // passes through untouched) and the heartbeat, which needs the runtime
                // the handler obtains at server init.
                |conn: Conn| async move {
                    let Some(shared) = shared_of(&conn) else {
                        return missing_state(conn);
                    };
                    match Include::parse(parse_param(conn.querystring(), "include")) {
                        Ok(include) => {
                            // `?since` past the head is a *wait*, not an error — the
                            // natural reading of a cursor on an append-only log, and
                            // the heartbeat is what keeps that wait distinguishable
                            // from a dead connection.
                            let since = parse_since(conn.querystring()).unwrap_or(0);
                            conn.with_state(EventsRequest {
                                shared,
                                include,
                                since,
                            })
                        }
                        Err(message) => fail(conn, ErrorCode::BadRequest, message),
                    }
                },
                sse(|conn: &mut Conn| match conn.take_state::<EventsRequest>() {
                    Some(EventsRequest {
                        shared,
                        include,
                        since,
                    }) => control_stream(shared, include, since),
                    // Unreachable: the validating half halts before this runs.
                    None => Box::pin(futures_lite::stream::empty()),
                })
                .with_heartbeat(heartbeat),
            ),
        )
}

/// The validated inputs of one `/events` request, handed from the route's validating
/// half to its `Sse` half through conn state.
struct EventsRequest {
    shared: Arc<Shared>,
    include: Include,
    since: u64,
}

/// [`router_with_heartbeat`] at the real [`EVENTS_HEARTBEAT`] interval.
pub(crate) fn router() -> Router {
    router_with_heartbeat(EVENTS_HEARTBEAT)
}

/// Which window of the log a `GET /log` read wants (docs/control-api.md).
///
/// `since`/`before` bound the range; `tail` takes the last N *of that range*. Splitting
/// position from count is why `?tail=N` is its own parameter rather than a negative
/// `since`: a position and a count are different kinds, and a parser cannot tell a
/// deliberate `since=-300` from a client's off-by-one `-1`.
///
/// Three reads follow from the three fields: `?since=S` resumes forward, `?tail=N` opens
/// cold at the tail, and `?before=X&tail=N` is one page backward — the escape hatch for a
/// client scrolling past the window it opened with.
#[derive(Default, PartialEq, Eq, Debug)]
pub(crate) struct LogSelect {
    /// Inclusive lower bound: entries with `seq >= since`.
    pub(crate) since: Option<u64>,
    /// Exclusive upper bound: entries with `seq < before`.
    pub(crate) before: Option<u64>,
    /// Keep only the last `tail` entries of the bounded range. Exactly N, cut anywhere:
    /// a cut can orphan a reference (a `tool_result` whose call sits in the entry before
    /// the cut), and those resolve by fetching `/log/:seq` or `/tool_calls/:call_id`
    /// rather than by widening the window.
    pub(crate) tail: Option<u64>,
}

impl LogSelect {
    /// Parse the `GET /log` query, or say what was wrong with it.
    ///
    /// `since` and `tail` together are an **error**, not a resolved precedence: one asks
    /// to resume from a position and the other to open at the end, so a request carrying
    /// both has no single reading, and picking one silently would answer a question the
    /// client didn't ask.
    pub(crate) fn parse(query: &str) -> Result<Self, String> {
        let select = Self {
            since: parse_u64_param(query, "since")?,
            before: parse_u64_param(query, "before")?,
            tail: parse_u64_param(query, "tail")?,
        };
        if select.since.is_some() && select.tail.is_some() {
            return Err("since and tail are mutually exclusive".into());
        }
        Ok(select)
    }
}

/// One `name=<u64>` query parameter, absent (`Ok(None)`) or malformed (`Err`).
///
/// Unlike [`parse_since`], a value that doesn't parse is rejected rather than ignored: on
/// a windowed read, a dropped bound silently returns the *whole log*, which is the
/// failure the window exists to prevent.
fn parse_u64_param(query: &str, name: &str) -> Result<Option<u64>, String> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
        .map(|v| {
            v.parse()
                .map_err(|_| format!("{name} must be a non-negative integer, got {v:?}"))
        })
        .transpose()
}

/// One `name=<value>` query parameter as a raw string slice, or `None` if absent.
pub(crate) fn parse_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
}

/// Pull `since=<n>` out of a raw query string (`GET /events?since=12`).
pub(crate) fn parse_since(query: &str) -> Option<u64> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("since="))
        .and_then(|v| v.parse().ok())
}

/// Whether a request asked to block until settled (`?wait`, `?wait=1`, `?wait=true`) — the
/// long-poll opt-in on the write routes (`/turns`, `/approvals`) (docs/permissions.md).
pub(crate) fn wants_wait(query: &str) -> bool {
    query
        .split('&')
        .any(|kv| matches!(kv, "wait" | "wait=1" | "wait=true"))
}

/// Block until the daemon settles (capped by [`SETTLE_TIMEOUT`]), then answer with the `/state`
/// snapshot — the shared tail of `GET /state?wait=settled` and the write routes' `?wait`. The
/// body is exactly `/state`'s, so a settled response is just a fresh state read; the client
/// inspects `gate` to tell a gate from idle.
pub(crate) async fn settle_and_state(shared: &Arc<Shared>, conn: Conn) -> Conn {
    let _ = tokio::time::timeout(SETTLE_TIMEOUT, shared.await_settled()).await;
    conn.with_status(200).with_body(shared.state_json()).halt()
}
