//! A turn that names the question it answers (docs/control-api.md).

use super::*;
use crate::log::{AssistantOutput, ContentBlock, RawResult, WireState};
use crate::persist::Log;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

fn turn(text: &str) -> SubmittedTurn {
    SubmittedTurn::new(text, crate::daemon::mint_submission_id())
        .sourced(Some("session[parent]".into()))
}

/// Log an assistant turn issuing an `ask` with `call_id`; returns its seq, the handle.
fn asks(shared: &Session, call_id: &str, question: &str) -> Seq {
    let call = ToolCall::new(
        crate::process_monitor::ASK,
        call_id,
        serde_json::json!({ "question": question }),
    );
    shared
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text("one question".into())],
                    vec![call],
                ),
                excluded: None,
            },
        )
        .unwrap()
}

/// While a question is pending, a turn naming none is refused with the question and
/// its handle, and the question is still pending after.
#[tokio::test]
async fn a_turn_naming_no_question_is_refused_while_one_is_pending() {
    let shared = session();
    shared.append(1, Payload::User(vec![])).unwrap();
    let seq = asks(&shared, "toolu_q", "Delete the old migrations?");
    let _wait = shared.begin_ask("toolu_q", "Delete the old migrations?".into(), 1, 1);
    let refusal = shared.submit_named_turn(turn("yes"), None).unwrap_err();
    assert_eq!(
        refusal.message("child", None),
        format!(
            "not delivered: child is asking ({seq}) \"Delete the old migrations?\" — reply with \
             reply_to: \"{seq}\""
        )
    );
    assert!(shared.asking().is_some(), "still asking");
    assert!(shared.queued_turns().is_empty(), "and nothing was queued");
}

/// A turn naming the pending question answers it.
#[tokio::test]
async fn a_turn_naming_the_pending_question_answers_it() {
    let shared = session();
    let seq = asks(&shared, "toolu_q", "Proceed?");
    let mut wait = shared.begin_ask("toolu_q", "Proceed?".into(), 1, 1);
    let landing = shared
        .submit_named_turn(turn("go ahead"), Some(&seq.to_string()))
        .unwrap();
    assert_eq!(landing, Landing::Answered);
    let reply = wait.reply().await.expect("the reply");
    assert_eq!(reply.text, "go ahead");
    assert_eq!(reply.source.as_deref(), Some("session[parent]"));

    // The call id names it as well as the handle does.
    let second = asks(&shared, "toolu_r", "And this?");
    drop(wait);
    let mut wait = shared.begin_ask("toolu_r", "And this?".into(), 1, 1);
    assert_eq!(
        shared.submit_named_turn(turn("yes"), Some("toolu_r")),
        Ok(Landing::Answered),
        "{second}"
    );
    assert!(wait.reply().await.is_some());
}

/// A reply naming a question that was already answered is refused with who answered it
/// and what they said — never delivered as a new message.
#[tokio::test]
async fn a_stale_reply_is_refused_naming_who_answered() {
    let shared = session();
    let seq = asks(&shared, "toolu_q", "Delete the old migrations?");
    shared
        .append_submission(
            1,
            Payload::tool_result("toolu_q", RawResult::text("no, keep them"), WireState::Raw),
            "sub-1".into(),
            None,
        )
        .unwrap();
    let refusal = shared
        .submit_named_turn(turn("yes, delete them"), Some(&seq.to_string()))
        .unwrap_err();
    assert_eq!(
        refusal.message("child", Some("session[parent]")),
        format!(
            "not delivered: child's question {seq} \"Delete the old migrations?\" was already \
             answered by operator: \"no, keep them\"; to send this as a new message instead, \
             leave out reply_to"
        )
    );
    assert!(shared.queued_turns().is_empty(), "nothing was queued");
}

/// A reply naming something that is not a question is refused, and so is a blank one.
#[tokio::test]
async fn a_reply_to_a_non_question_or_a_blank_reply_is_refused() {
    let shared = session();
    let seq = shared
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new(
                        crate::bash::NAME,
                        "toolu_b",
                        serde_json::json!({ "command": "ls" }),
                    )],
                ),
                excluded: None,
            },
        )
        .unwrap();
    let refusal = shared
        .submit_named_turn(turn("ok"), Some(&seq.to_string()))
        .unwrap_err();
    assert_eq!(
        refusal.message("child", None),
        format!(
            "not delivered: {seq} is a bash call, not a question; reply_to takes the seq of a \
             question child asked, as its status gives it"
        )
    );
    assert_eq!(
        shared.submit_named_turn(turn("  "), Some(&seq.to_string())),
        Err(NotDelivered::Blank)
    );
}

/// With no question pending, a turn naming none lands as a turn: at once on an idle
/// loop.
#[tokio::test]
async fn a_turn_naming_none_lands_when_nothing_is_asked() {
    let shared = session();
    assert_eq!(
        shared.submit_named_turn(turn("hello"), None),
        Ok(Landing::Now)
    );
    assert_eq!(shared.queued_turns().len(), 1);
    assert_eq!(
        shared.submit_named_turn(turn("and this"), None),
        Ok(Landing::Queued),
        "behind the first"
    );
}
