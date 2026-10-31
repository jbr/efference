//! `/turns` and `/approvals`: submitting a user turn, recalling one, and resolving the
//! approval gate (docs/control-api.md, docs/permissions.md).

use super::*;
use crate::protocol::answers::{StatusBody, StatusWord};
use crate::protocol::turns::{Landing, QueuedTurn, TurnAccepted, WithdrawnTurns};

/// Enqueue a user turn (also seeds turn 1). 202 — watch the stream for the
/// response; submission is decoupled from inference. A session still starting queues
/// it, and it is the first turn the session runs (`starting.rs`).
///
/// A turn answers a pending `ask` only by naming it, from every client, and is refused
/// otherwise (`named_turns.rs`): 409 `not_delivered`, the pending ask in `detail`. So a
/// message written before the question appeared is never read as its answer. The 202
/// says where the turn went (`delivery`).
pub(super) async fn submit_turn(mut conn: Conn) -> Conn {
    let shared = shared_of(&conn);
    let starting = starting_of(&conn);
    if shared.is_none() && starting.is_none() {
        return missing_state(conn);
    }
    let wait = wants_wait(conn.querystring());
    let body = conn.request_body_string().await.unwrap_or_default();
    let parsed = match parse_turn(&body) {
        Ok(turn) => turn,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let TurnBody {
        text,
        submission_id,
        source,
        attachments,
        answers,
    } = parsed;
    let submission_id = submission_id.unwrap_or_else(mint_submission_id);
    let turn = SubmittedTurn::new(text, submission_id.clone()).sourced(source.clone());
    let landing = match (shared, starting) {
        (Some(shared), _) => {
            let attachments = match resolve_attachments(&shared, &attachments) {
                Ok(attachments) => attachments,
                Err(message) => return fail(conn, ErrorCode::BadRequest, message),
            };
            let turn = turn.with_attachments(attachments);
            let landing = match shared.submit_named_turn(turn, answers.as_deref()) {
                Ok(landing) => landing,
                Err(refusal) => {
                    let id = shared.identity().map(|identity| identity.id);
                    let message =
                        refusal.message(id.as_deref().unwrap_or("the session"), source.as_deref());
                    return respond(
                        conn,
                        &ErrorBody::new(ErrorCode::NotDelivered, message)
                            .with_detail(shared.topic_json(Topic::Ask)),
                    );
                }
            };
            // `?wait`: hold the connection until the turn reaches a gate or completes,
            // then answer with the resulting state — "act → read state" in one
            // round-trip (docs/permissions.md).
            if wait {
                return settle_and_state(&shared, conn).await;
            }
            landing
        }
        (None, Some(starting)) => {
            // The attachment store is the session's, and exists once it has started.
            if !attachments.is_empty() {
                let refusal = starting.refusal();
                return fail(conn, ErrorCode::Starting, refusal);
            }
            // A starting session has asked nothing, so a named question is not pending.
            if let Some(reference) = answers {
                let message = format!(
                    "not delivered: {} is still starting and has asked nothing; {reference} \
                     names no question it is waiting on",
                    starting.id()
                );
                return respond(
                    conn,
                    &ErrorBody::new(ErrorCode::NotDelivered, message).with_detail(Value::Null),
                );
            }
            if let Err(refusal) = starting.submit(turn) {
                return fail(conn, ErrorCode::Starting, refusal);
            }
            if wait {
                return settle_starting(&starting, conn).await;
            }
            Landing::Queued
        }
        (None, None) => return missing_state(conn),
    };
    // The id is informational here: the turn's effects — including the `User` entry
    // carrying this id — can arrive on the stream before this body resolves, so
    // clients must not sequence state on it (docs/control-api.md).
    views::answer(
        conn,
        202,
        TurnAccepted {
            status: StatusWord::Accepted,
            submission_id,
            delivery: landing,
        },
    )
}

/// Parse a `POST /turns` body and check what a body alone can be refused for.
fn parse_turn(body: &str) -> Result<TurnBody, String> {
    let turn =
        serde_json::from_str::<TurnBody>(body).map_err(|e| format!("bad /turns body: {e}"))?;
    if turn
        .submission_id
        .as_ref()
        .is_some_and(|id| id.len() > SUBMISSION_ID_MAX || id.is_empty())
    {
        return Err(format!("submission_id must be 1–{SUBMISSION_ID_MAX} bytes"));
    }
    // The kickoff's source is the harness's alone: the clients leave a turn carrying
    // it out of the transcript, so a client claiming it would be submitting a turn the
    // operator never sees (docs/context.md).
    if turn.source.as_deref() == Some(crate::frontend::KICKOFF_SOURCE) {
        return Err(format!(
            "source `{}` is reserved for the harness's first turn",
            crate::frontend::KICKOFF_SOURCE
        ));
    }
    // Likewise a hand-off brief's: the clients render it as the predecessor's brief,
    // which only `hand_off` writes (docs/tools.md).
    if turn
        .source
        .as_deref()
        .and_then(crate::frontend::handoff_predecessor)
        .is_some()
    {
        return Err("a `handoff[…]` source is reserved for a hand-off brief".to_string());
    }
    Ok(turn)
}

/// Describe each named upload as the turn will carry it. Described at submission rather than when
/// the loop dequeues the turn, so a refusal reaches the client that sent it.
fn resolve_attachments(
    shared: &Session,
    names: &[String],
) -> Result<Vec<crate::log::attachment::Arrival>, String> {
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
    let shared = shared_of(&conn);
    let starting = starting_of(&conn);
    let Some(id) = conn.param("submission_id").map(str::to_string) else {
        return fail(conn, ErrorCode::BadRequest, "missing submission id");
    };
    let popped = match (shared, starting) {
        (Some(shared), _) => shared.withdraw_turns(&id),
        (None, Some(starting)) => starting.withdraw(&id),
        (None, None) => return missing_state(conn),
    };
    let withdrawn: Vec<QueuedTurn> = popped.iter().map(QueuedTurn::from).collect();
    views::answer(conn, 200, WithdrawnTurns { withdrawn })
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
    let decider = Decider::default()
        .with_source(decision.source)
        .with_note_submission(decision.note_submission);
    if shared.resolve_approval(&id, approval, decider) {
        // `?wait`: hold until the turn reaches its *next* gate or completes — so a
        // multi-gate turn is driven as a clean "approve → read state" loop.
        if wait {
            settle_and_state(&shared, conn).await
        } else {
            views::answer(conn, 200, StatusBody::new(StatusWord::Resolved))
        }
    } else {
        // The precondition failed: that id is not the pending gate — already
        // resolved, or the loop moved on. 409 with the *current* gate as
        // corrective detail (`null` = none pending), so a client re-renders
        // without a second round-trip — an unqualified 404 could not tell
        // "that gate moved" from "there is no such call".
        respond(
            conn,
            &ErrorBody::new(
                ErrorCode::GateMoved,
                "that call is not the pending approval gate",
            )
            .with_detail(shared.topic_json(Topic::Gate)),
        )
    }
}

/// Escalate the pending approval gate to the operator (docs/permissions.md): the call
/// stays held, and the gate topic, the status and a push say who escalated it and the
/// note. The route is an operator client's, so the escalation goes to the operator. The
/// id is a precondition, as on `PUT /approvals/:call_id`.
pub(super) async fn escalate_approval(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let id = CallId(conn.param("call_id").unwrap_or_default().to_string());
    let body = conn.request_body_string().await.unwrap_or_default();
    let body = match serde_json::from_str::<EscalationBody>(&body) {
        Ok(body) => body,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad escalation body: {e}"),
            );
        }
    };
    let note = body.note.filter(|note| !note.trim().is_empty());
    if shared.escalate(&id, body.source, None, note) {
        views::answer(conn, 200, StatusBody::new(StatusWord::Escalated))
    } else {
        respond(
            conn,
            &ErrorBody::new(
                ErrorCode::GateMoved,
                "that call is not the pending approval gate",
            )
            .with_detail(shared.topic_json(Topic::Gate)),
        )
    }
}
