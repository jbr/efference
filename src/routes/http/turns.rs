//! `/turns` and `/approvals`: submitting a user turn, recalling one, and resolving the
//! approval gate (docs/control-api.md, docs/permissions.md).

use super::*;
use crate::host::Host;
use crate::protocol::answers::{StatusBody, StatusWord};
use crate::protocol::turns::{Landing, QueuedTurn, TurnAccepted, WithdrawnTurns};
use crate::routes::host_http::host_of;
use crate::session::Decider;
use crate::session::Session;

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
        Ok(parsed) => parsed,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let ParsedTurn {
        form,
        submission_id,
        source,
        attachments,
        answers,
    } = parsed;
    let submission_id = submission_id.unwrap_or_else(mint_submission_id);
    let landing = match (shared, starting) {
        (Some(shared), _) => {
            // Both spellings normalize here, into one submitted document: the
            // legacy form resolves its uploads and attaches them after the
            // prose; the structured form validates in full before anything
            // queues (docs/control-api.md).
            let turn = match submitted_turn(
                Some(&shared),
                host_of(&conn).as_deref(),
                form,
                &attachments,
                &submission_id,
                source.as_deref(),
            ) {
                Ok(turn) => turn,
                Err(message) => return fail(conn, ErrorCode::BadRequest, message),
            };
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
            // The attachment store is the session's, and exists once it has started;
            // both spellings refuse to attach before that.
            if names_an_upload(&form, &attachments) {
                let refusal = starting.refusal();
                return fail(conn, ErrorCode::Starting, refusal);
            }
            // A starting session has asked nothing, so a named question is not pending.
            if let Some(reference) = answers {
                let message = format!(
                    "not delivered: {} is still starting and has asked nothing; {reference} \
                     does not identify a question it is waiting on",
                    starting.id()
                );
                return respond(
                    conn,
                    &ErrorBody::new(ErrorCode::NotDelivered, message).with_detail(Value::Null),
                );
            }
            let turn = match submitted_turn(
                None,
                host_of(&conn).as_deref(),
                form,
                &attachments,
                &submission_id,
                source.as_deref(),
            ) {
                Ok(turn) => turn,
                Err(message) => return fail(conn, ErrorCode::BadRequest, message),
            };
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

/// The body's spelling, normalized at the gate: the legacy prose form, or the
/// structured document form (docs/control-api.md). `parse_turn` guarantees one
/// of the two and nothing else.
enum TurnForm {
    Legacy {
        text: String,
    },
    Structured {
        content: Vec<UserSegment>,
        references: BTreeMap<String, SubmittedReference>,
    },
}

/// A parsed `POST /turns` body: the spelling it normalized to, plus the
/// metadata every spelling carries (docs/control-api.md). Gate-internal —
/// [`TurnBody`] is the wire shape, this is what the gate works with.
struct ParsedTurn {
    form: TurnForm,
    submission_id: Option<String>,
    source: Option<String>,
    attachments: Vec<String>,
    answers: Option<String>,
}

/// Parse a `POST /turns` body and check what a body alone can be refused for —
/// including which spelling it is: the legacy `text` (with optional
/// `attachments`), or the structured `content` with `references` — never both,
/// never neither — and a structured turn may not also carry `attachments`
/// (docs/control-api.md).
fn parse_turn(body: &str) -> Result<ParsedTurn, String> {
    let turn =
        serde_json::from_str::<TurnBody>(body).map_err(|e| format!("bad /turns body: {e}"))?;
    let TurnBody {
        text,
        content,
        references,
        submission_id,
        source,
        attachments,
        answers,
    } = turn;
    let form =
        match (text, content, references) {
            (Some(text), None, None) => TurnForm::Legacy { text },
            (None, Some(content), Some(references)) => TurnForm::Structured {
                content,
                references,
            },
            (None, None, None) => return Err("the turn carries no text".to_string()),
            _ => return Err(
                "submit either `text` (with optional `attachments`) or `content` together with \
                 `references`, not both"
                    .to_string(),
            ),
        };
    // A structured turn names its uploads as `references`; a separate list
    // would be a second, unchecked spelling of the same thing, so it is
    // refused rather than ignored.
    if matches!(&form, TurnForm::Structured { .. }) && !attachments.is_empty() {
        return Err(
            "a structured turn carries its attachments as `content` occurrences with \
             `references` definitions, not a separate `attachments` list"
                .to_string(),
        );
    }
    if submission_id
        .as_ref()
        .is_some_and(|id| id.len() > SUBMISSION_ID_MAX || id.is_empty())
    {
        return Err(format!("submission_id must be 1–{SUBMISSION_ID_MAX} bytes"));
    }
    // The kickoff's source is the harness's alone: the clients leave a turn carrying
    // it out of the transcript, so a client claiming it would be submitting a turn the
    // operator never sees (docs/context.md).
    if source.as_deref() == Some(crate::record::names::sources::KICKOFF_SOURCE) {
        return Err(format!(
            "source `{}` is reserved for the harness's first turn",
            crate::record::names::sources::KICKOFF_SOURCE
        ));
    }
    // Likewise a hand-off brief's: the clients render it as the predecessor's brief,
    // which only `hand_off` writes (docs/tools.md).
    if source
        .as_deref()
        .and_then(crate::record::names::sources::handoff_predecessor)
        .is_some()
    {
        return Err("a `handoff[…]` source is reserved for a hand-off brief".to_string());
    }
    Ok(ParsedTurn {
        form,
        submission_id,
        source,
        attachments,
        answers,
    })
}

/// Describe each named upload as the turn will carry it. Described at submission rather than when
/// the loop dequeues the turn, so a refusal reaches the client that sent it.
fn resolve_attachments(
    shared: &Session,
    names: &[String],
) -> Result<Vec<crate::record::log::attachment::Arrival>, String> {
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

/// Normalize either body spelling into the turn it submits (docs/control-api.md):
/// the legacy form resolves its uploads against the session, the structured form
/// validates in full. `session` is `None` for a session still starting, which has
/// no store to resolve against — and cannot reach here with attachments, since
/// that route refuses them first.
fn submitted_turn(
    session: Option<&Session>,
    host: Option<&Host>,
    form: TurnForm,
    attachments: &[String],
    submission_id: &str,
    source: Option<&str>,
) -> Result<SubmittedTurn, String> {
    match form {
        TurnForm::Legacy { text } => {
            let arrivals = match session {
                Some(session) => resolve_attachments(session, attachments)?,
                None if attachments.is_empty() => Vec::new(),
                None => return Err("attachments need a started session".to_string()),
            };
            Ok(SubmittedTurn::new(text, submission_id)
                .sourced(source.map(str::to_string))
                .with_attachments(arrivals))
        }
        TurnForm::Structured {
            content,
            references,
        } => {
            let document = validate_document(session, host, content, references)?;
            Ok(SubmittedTurn::structured(document, submission_id)
                .sourced(source.map(str::to_string)))
        }
    }
}

/// Whether either spelling names an upload: the legacy `attachments` list, or a
/// structured reference whose target is an attachment.
fn names_an_upload(form: &TurnForm, attachments: &[String]) -> bool {
    match form {
        TurnForm::Legacy { .. } => !attachments.is_empty(),
        TurnForm::Structured { references, .. } => references
            .values()
            .any(|reference| matches!(reference.target, SubmittedTarget::Attachment { .. })),
    }
}

/// Validate a structured submission and resolve it into the stored document
/// (docs/control-api.md): ids checked and preserved verbatim — never renamed —
/// every occurrence resolved to a definition, no unused definitions (a
/// definition with no occurrence still names a target the model would not
/// read), attachment definitions used exactly once
/// and resolved against the session's store, agent identities confirmed
/// against the host's live registry, and filesystem targets refused.
fn validate_document(
    session: Option<&Session>,
    host: Option<&Host>,
    content: Vec<UserSegment>,
    references: BTreeMap<String, SubmittedReference>,
) -> Result<UserContent, String> {
    for id in references.keys() {
        if id.is_empty()
            || id.len() > REFERENCE_ID_MAX
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(format!(
                "reference id `{id}` must be 1–{REFERENCE_ID_MAX} bytes of ASCII letters, \
                 digits, `.`, `-` or `_`"
            ));
        }
        if references[id].label.is_empty() || references[id].label.len() > REFERENCE_LABEL_MAX {
            return Err(format!(
                "reference `{id}` label must be 1–{REFERENCE_LABEL_MAX} bytes"
            ));
        }
    }
    // Every occurrence resolves, and the count per id feeds the exactly-once
    // rule for attachments.
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for segment in &content {
        if let UserSegment::Reference(id) = segment {
            if !references.contains_key(id) {
                return Err(format!("reference `{id}` is used but not defined"));
            }
            *counts.entry(id.as_str()).or_default() += 1;
        }
    }
    let mut stored = BTreeMap::new();
    // The same store name under two ids would post the upload twice, so the
    // exactly-once rule spans definitions, not just one id's occurrences.
    let mut attachment_names: BTreeMap<String, String> = BTreeMap::new();
    for (id, reference) in references {
        let count = counts.get(id.as_str()).copied().unwrap_or(0);
        if count == 0 {
            return Err(format!("reference `{id}` is defined but never used"));
        }
        let definition = match reference.target {
            SubmittedTarget::Filesystem { .. } => {
                return Err("filesystem references are not accepted yet".to_string());
            }
            SubmittedTarget::Attachment { name } => {
                if count > 1 {
                    return Err(format!(
                        "attachment `{name}` is referenced {count} times; each upload is \
                         referenced exactly once"
                    ));
                }
                if let Some(first) = attachment_names.get(name.as_str()) {
                    return Err(format!(
                        "attachment `{name}` is defined as both `{first}` and `{id}`; each \
                         upload is referenced exactly once"
                    ));
                }
                attachment_names.insert(name.clone(), id.clone());
                let store = session
                    .and_then(Session::composer_store)
                    .ok_or("this session has no attachment store")?;
                let arrival = store
                    .arrival(&name)
                    .map_err(|e| format!("attachment `{name}`: {e}"))?;
                ReferenceDefinition::attachment(arrival)
            }
            SubmittedTarget::Agent { session: agent } => {
                let known = host.is_some_and(|host| host.listed(&agent).is_some());
                if !known {
                    return Err(match host {
                        Some(_) => format!("unknown agent session `{agent}` on this host"),
                        None => "agent references need the host registry; submit through the host \
                             channel"
                            .to_string(),
                    });
                }
                ReferenceDefinition::agent(reference.label, agent)
            }
        };
        stored.insert(id, definition);
    }
    Ok(UserContent {
        content,
        references: stored,
    })
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
