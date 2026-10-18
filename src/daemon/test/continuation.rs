//! Tests for `continuation.rs` — the zero-token continue (docs/control-api.md).

use super::*;
use crate::client::ContinueOutcome;

/// An assistant turn with prose and no calls — a finished turn's wire tail.
fn assistant_text(text: &str) -> Payload {
    Payload::Assistant {
        output: crate::log::AssistantOutput::from_legacy_messages(
            vec![],
            vec![ContentBlock::Text(text.into())],
            vec![],
        ),
        excluded: None,
    }
}

/// An assistant turn carrying one unanswered tool call.
fn assistant_calling(call_id: &str) -> Payload {
    Payload::Assistant {
        output: crate::log::AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            vec![ToolCall::new(
                crate::bash::NAME,
                call_id,
                serde_json::json!({ "command": "ls" }),
            )],
        ),
        excluded: None,
    }
}

/// The paired result — a `tool_result` tail, which projects as a **user** message.
fn tool_result(call_id: &str) -> Payload {
    Payload::tool_result(
        call_id,
        crate::log::RawResult::text("ok"),
        crate::log::WireState::Raw,
    )
}

/// A record-only assistant entry: an upstream attempt that died before finalizing.
/// It projects to nothing (docs/log-and-projection.md), so it must not change what
/// the tail looks like to the continue precondition.
fn aborted_attempt() -> Payload {
    Payload::Assistant {
        output: crate::log::AssistantOutput::Messages(vec![]),
        excluded: Some(crate::log::Exclusion::Upstream("stream aborted".into())),
    }
}

/// The continuable-tail precondition reads the **wire**, not the log: a tool_result
/// tail and a plain user tail continue; a finished assistant turn and an unanswered
/// `tool_use` do not.
#[test]
fn continuable_tail_is_a_question_about_the_wire() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    // An empty log has no wire to re-prompt.
    assert_eq!(
        shared
            .request_continue(Default::default())
            .unwrap_err()
            .message(),
        "nothing to continue: the session has no turns yet"
    );

    // A user turn the model never answered — the motivating case.
    shared.append(1, user("hello")).unwrap();
    assert!(shared.request_continue(Default::default()).is_ok());
    shared.take_continue();
    shared.going_idle();

    // A finished turn declines.
    shared.append(1, assistant_text("done")).unwrap();
    assert_eq!(
        shared
            .request_continue(Default::default())
            .unwrap_err()
            .message(),
        "nothing to continue: the last turn completed"
    );

    // An unanswered `tool_use` is a shape the API rejects, so it declines too.
    shared.append(2, user("more")).unwrap();
    shared.append(2, assistant_calling("toolu_a")).unwrap();
    assert_eq!(
        shared
            .request_continue(Default::default())
            .unwrap_err()
            .message(),
        "nothing to continue: the last tool call has no result"
    );

    // Its result lands: the wire ends in a user-role message again.
    shared.append(2, tool_result("toolu_a")).unwrap();
    assert!(shared.request_continue(Default::default()).is_ok());
}

/// A finished turn declines in every dialect. The Messages encoding drops Responses and
/// Completions assistant turns, so a precondition read from it saw every such tail as
/// an unanswered user turn and accepted the continue (observed 2026-10-17 on a Codex
/// session).
#[test]
fn a_finished_turn_declines_in_every_dialect() {
    use crate::log::{
        AssistantOutput, CompletionsMessage, ResponsesMessageContent, ResponsesOutputItem,
    };
    let responses_text = AssistantOutput::Responses(vec![ResponsesOutputItem::Message {
        id: "msg_1".into(),
        status: "completed".into(),
        phase: None,
        content: vec![ResponsesMessageContent::OutputText {
            text: "done".into(),
            annotations: vec![],
        }],
    }]);
    let serde_json::Value::Object(message) =
        serde_json::json!({ "role": "assistant", "content": "done" })
    else {
        unreachable!()
    };
    let completions_text = AssistantOutput::Completions(CompletionsMessage::from_map(message));
    for output in [responses_text, completions_text] {
        let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
        shared.append(1, user("hello")).unwrap();
        shared
            .append(
                1,
                Payload::Assistant {
                    output,
                    excluded: None,
                },
            )
            .unwrap();
        assert_eq!(
            shared
                .request_continue(Default::default())
                .unwrap_err()
                .message(),
            "nothing to continue: the last turn completed"
        );
    }
}

/// Record-only entries project to nothing, so a trajectory that gave up mid-stream
/// still shows the tail it had before the failure — which is what makes the
/// stalled session continuable rather than "completed".
#[test]
fn record_only_entries_do_not_move_the_tail() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("hello")).unwrap();
    shared.append(2, aborted_attempt()).unwrap();
    shared.append(3, aborted_attempt()).unwrap();
    assert!(
        shared.request_continue(Default::default()).is_ok(),
        "a stalled trajectory is continuable — the wire still ends at the user turn"
    );
}

/// A continue is declined while a turn is in flight and while a gate is open: in
/// neither state is control the operator's to hand back.
#[test]
fn a_running_or_gated_loop_declines() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("hello")).unwrap();

    shared.submit_turn(SubmittedTurn::new("hi", "sub-test"));
    assert_eq!(
        shared
            .request_continue(Default::default())
            .unwrap_err()
            .message(),
        "the loop is already running — there is nothing to hand back"
    );
    shared.claim_turn("sub-test", true);
    assert!(
        shared.request_continue(Default::default()).is_err(),
        "mid-turn declines"
    );

    let call = bash_call("ls");
    let _gate = shared.register_pending(&call, None);
    assert_eq!(
        shared
            .request_continue(Default::default())
            .unwrap_err()
            .message(),
        "an approval gate is pending — resolve it to continue"
    );
}

/// An accepted continue is a turn about to run: the daemon is not at a decision
/// point until the loop has consumed it.
#[test]
fn a_pending_continue_is_not_settled() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("hello")).unwrap();
    assert!(shared.is_settled());
    shared.request_continue(Default::default()).unwrap();
    assert!(!shared.is_settled(), "a pending continue reads as busy");
    // Idempotent: two presses are one continue.
    assert!(shared.request_continue(Default::default()).is_ok());
    shared.take_continue();
    assert!(!shared.continue_pending(), "consumed exactly once");
}

/// `continue_wake` resolves when a continue is requested — including one that
/// lands before the waiter registers, which the flag (not the notify) carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continue_wake_resolves_on_request() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("hello")).unwrap();

    let waiter = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { shared.continue_wake().await })
    };
    // Racy on purpose: whether this lands before or after the waiter registers,
    // the flag is what the wake is carried by.
    shared.request_continue(Default::default()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the wake arrives")
        .expect("the waiter task did not panic");
}

/// A continue appends nothing the model reads: only its own record, and the projected
/// wire is byte-identical either side of the request, which is what "zero-token" means.
#[test]
fn a_continue_appends_only_its_record() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("hello")).unwrap();
    let before = shared.snapshot();
    let wire = serde_json::to_string(&shared.project_messages()).unwrap();

    shared.request_continue(Default::default()).unwrap();
    shared.take_continue();

    let after = shared.snapshot();
    assert_eq!(after.len(), before.len() + 1, "one entry was added");
    assert!(matches!(
        after.last().unwrap().payload,
        Payload::Telemetry(crate::log::Telemetry::Continue(_))
    ));
    assert_eq!(
        serde_json::to_string(&shared.project_messages()).unwrap(),
        wire,
        "the wire is byte-identical across a continue"
    );
}

/// The route: 202 on accept, `409 not_continuable` on a decline, with the reason
/// in the envelope's `message`.
#[tokio::test]
async fn the_route_accepts_and_declines() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(1, user("hello")).unwrap();
    let client = test_client(&shared).await;

    assert_eq!(
        client.continue_loop(None).await.unwrap(),
        ContinueOutcome::Accepted
    );
    shared.take_continue();
    shared.going_idle();

    shared.append(1, assistant_text("done")).unwrap();
    assert_eq!(
        client.continue_loop(None).await.unwrap(),
        ContinueOutcome::Declined {
            message: "nothing to continue: the last turn completed".into()
        }
    );
}

/// The decline carries the typed envelope, so a client branches on `code` rather
/// than on prose.
#[tokio::test]
async fn a_decline_carries_the_typed_envelope() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let server = test_server(&shared).await;
    let conn = server.post("/v1/loop/continue").await;
    conn.assert_status(409);
    conn.assert_json_body_with(|body: &Value| {
        assert_eq!(body["code"], "not_continuable");
        assert_eq!(
            body["message"],
            "nothing to continue: the session has no turns yet"
        );
    });
}

/// An accepted continue is recorded with who sent it, ahead of the request it causes, and
/// the wire does not move; a declined one writes nothing (docs/control-api.md).
#[test]
fn an_accepted_continue_records_who_sent_it() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let continues = |shared: &Session| -> Vec<Option<String>> {
        shared
            .snapshot()
            .iter()
            .filter_map(|e| match &e.payload {
                Payload::Telemetry(crate::log::Telemetry::Continue(origin)) => {
                    Some(origin.source.clone())
                }
                _ => None,
            })
            .collect()
    };
    // Declined: there is nothing to continue yet.
    assert!(shared.request_continue(Default::default()).is_err());
    assert!(
        continues(&shared).is_empty(),
        "a declined continue writes nothing"
    );

    shared.append(1, user("hello")).unwrap();
    let wire = serde_json::to_string(&shared.project_messages()).unwrap();
    let origin = crate::log::ContinueOrigin {
        source: Some("mcp[claude-code]".into()),
    };
    assert!(shared.request_continue(origin).is_ok());
    assert_eq!(
        continues(&shared),
        vec![Some("mcp[claude-code]".to_string())]
    );
    assert_eq!(
        serde_json::to_string(&shared.project_messages()).unwrap(),
        wire,
        "the record projects to nothing"
    );
}
