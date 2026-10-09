//! `/turns` and `/approvals`: submitting a user turn, recalling one, and resolving the
//! approval gate (docs/control-api.md, docs/permissions.md).

use super::*;

/// Enqueue a user turn (also seeds turn 1). 202 — watch the stream for the
/// response; submission is decoupled from inference.
pub(super) async fn submit_turn(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let wait = wants_wait(conn.querystring());
    let body = conn.request_body_string().await.unwrap_or_default();
    match serde_json::from_str::<TurnBody>(&body) {
        Ok(TurnBody {
            text,
            submission_id,
            source,
            attachments,
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
            // The kickoff's source is the harness's alone: the clients leave a turn
            // carrying it out of the transcript, so a client claiming it would be
            // submitting a turn the operator never sees (docs/context.md).
            if source.as_deref() == Some(crate::frontend::KICKOFF_SOURCE) {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!(
                        "source `{}` is reserved for the harness's first turn",
                        crate::frontend::KICKOFF_SOURCE
                    ),
                );
            }
            // Likewise a hand-off brief's: the clients render it as the
            // predecessor's brief, which only `hand_off` writes (docs/tools.md).
            if source
                .as_deref()
                .and_then(crate::frontend::handoff_predecessor)
                .is_some()
            {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "a `handoff[…]` source is reserved for a hand-off brief".to_string(),
                );
            }
            let attachments = match resolve_attachments(&shared, &attachments) {
                Ok(attachments) => attachments,
                Err(message) => return fail(conn, ErrorCode::BadRequest, message),
            };
            shared.person_acted(shared.turn_released_by());
            let submission_id = submission_id.unwrap_or_else(mint_submission_id);
            shared.submit_turn(
                SubmittedTurn::new(text, submission_id.clone())
                    .sourced(source)
                    .with_attachments(attachments),
            );
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
}

/// Describe each named upload as the turn will carry it. Described at submission rather than when
/// the loop dequeues the turn, so a refusal reaches the client that sent it.
fn resolve_attachments(
    shared: &Session,
    names: &[String],
) -> Result<Vec<crate::attachments::Arrival>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let store = shared
        .composer_store()
        .ok_or("this session has no attachment store")?;
    names
        .iter()
        .map(|name| {
            store
                .arrival(name)
                .map_err(|e| format!("attachment `{name}`: {e}"))
        })
        .collect()
}

/// Withdraw the pending-queue suffix beginning at this submission id — recall
/// (docs/control-api.md). Always 200: the loop may have dequeued the turn already,
/// and "popped nothing" is a race outcome the client renders, not an error. The
/// response is authoritative — a client splices only what came back.
pub(super) async fn withdraw_turns(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(id) = conn.param("submission_id").map(str::to_string) else {
        return fail(conn, ErrorCode::BadRequest, "missing submission id");
    };
    let withdrawn: Vec<Value> = shared
        .withdraw_turns(&id)
        .into_iter()
        .map(|t| t.pending_json())
        .collect();
    let body = serde_json::json!({ "withdrawn": withdrawn });
    conn.with_status(200).with_body(body.to_string()).halt()
}

/// Resolve the pending approval gate, addressed by the call awaiting it. A
/// `PUT` with the id as a real precondition: the decision applies only to
/// the gate the client is looking at.
pub(super) async fn resolve_approval(mut conn: Conn) -> Conn {
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
        // without a second round-trip — an unqualified 404 could not tell
        // "that gate moved" from "there is no such call".
        respond(
            conn,
            ErrorBody::new(
                ErrorCode::GateMoved,
                "that call is not the pending approval gate",
            )
            .with_detail(shared.topic_json(Topic::Gate)),
        )
    }
}
