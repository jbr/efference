//! `message` in-session: delivery by relation, the continue, and replies to questions
//! (docs/tools.md).

use super::*;
use crate::log::{RawResult, WireState};

/// A driver's message is the child's next user message, under the parent's
/// `session[<id>]` provenance, which opens the turn as `parent (agent):` on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drivers_message_lands_as_a_turn_from_it() {
    let fixture = Fixture::new().await;
    let said = message("child", "do the thing", None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .expect("the parent drives its child");
    assert_eq!(said, "delivered: child is working on it");

    let child = fixture.session("child");
    child
        .wait_until("the child logged the turn", || {
            child
                .snapshot()
                .iter()
                .any(|entry| entry.source.as_deref() == Some("session[parent]"))
        })
        .await;
    let wire = serde_json::to_string(&crate::project::project(&child.snapshot())).unwrap();
    assert!(wire.contains("parent (agent): do the thing"), "{wire}");

    fixture.host.swansong().shut_down().await;
}

/// A message from a session that does not drive its target is a note in the target's
/// inbox under the sender's id, and the answer says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peers_message_is_a_notification() {
    let fixture = Fixture::new().await;
    let said = message("peer", "the build is green", None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        "left in peer's inbox; it may set it aside (you don't drive it)"
    );
    let peer = fixture.session("peer");
    let heard = || {
        peer.pending_notifications()
            .iter()
            .any(|n| n.source == "parent" && n.body == "the build is green")
            || peer
                .snapshot()
                .iter()
                .any(|e| matches!(&e.payload, Payload::Ambient(n) if n.source == "parent"))
    };
    peer.wait_until("the note reached the peer", heard).await;
    assert!(
        !peer
            .snapshot()
            .iter()
            .any(|e| e.source.as_deref() == Some("session[parent]")),
        "no turn was submitted"
    );

    // An empty note continues nothing, and a reply from a non-driver answers nothing.
    let err = message("peer", "", None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("an empty message continues a session you drive"),
        "{err}"
    );
    let err = message("peer", "yes", Some("3"))
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("only a session that drives peer"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// An empty message from a driver is a continue: it releases a stalled session, and
/// at a completed turn it is the continue route's refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_message_continues_or_says_why_not() {
    let fixture = Fixture::new().await;
    let child = fixture.session("child");
    child.append(1, Payload::user("go")).unwrap();
    child.set_stall(Stall {
        reason: StallReason::RetriesExhausted,
        attempts: Some(12),
        at: 0,
        message: "the trajectory stopped: 12 attempts in a row failed upstream".into(),
        detail: String::new(),
    });
    let said = message("child", "", None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(said, "continuing: child goes on from where it stopped");

    let grandchild = fixture.session("grandchild");
    grandchild.append(1, Payload::user("go")).unwrap();
    grandchild
        .append(
            1,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text("done".into())],
                vec![],
            )),
        )
        .unwrap();
    let err = message("grandchild", "  ", None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "not continued: nothing to continue: the last turn completed"
    );

    fixture.host.swansong().shut_down().await;
}

/// A message to a session holding a call waits in the queue, and the answer names the
/// decision it waits behind; it never resolves the gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_queues_behind_a_gate() {
    let fixture = Fixture::new().await;
    let child = fixture.session("child");
    let seq = issue(&child, vec![curl()]);
    let _gate = child.register_pending(&curl(), None, None);
    let said = message("child", "also check the logs", None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        format!(
            "queued: child is waiting for a decision on {seq} (bash `curl -sI \
             https://example.com`); it reads this after the decision"
        )
    );
    assert!(child.pending_gate().is_some(), "the gate is still held");

    fixture.host.swansong().shut_down().await;
}

/// While the child asks, a message without `reply_to` is refused with the question;
/// one naming it answers it; and a reply naming it once it was answered is refused,
/// saying who answered and how.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn questions_are_answered_by_naming_them() {
    let fixture = Fixture::new().await;
    let child = fixture.session("child");
    let ask = ToolCall::new(
        crate::log::ASK,
        "toolu_q",
        serde_json::json!({ "question": "Delete the old migrations?" }),
    );
    let seq = issue(&child, vec![ask]).to_string();
    let mut wait = child.begin_ask("toolu_q", "Delete the old migrations?".into(), 1, 1);
    let parent = fixture.control("parent");

    let err = message("child", "yes", None)
        .execute(parent.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "not delivered: child is asking ({seq}) \"Delete the old migrations?\" — reply \
             with reply_to: \"{seq}\""
        )
    );

    let said = message("child", "no, keep them", Some(&seq))
        .execute(parent.clone(), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        format!(
            "answered: child has your answer to {seq} \"Delete the old migrations?\" and goes on"
        )
    );
    let reply = wait.reply().await.expect("the reply");
    assert_eq!(reply.text, "no, keep them");
    drop(wait);
    // The loop logs the reply as the ask's result, under its sender.
    child
        .append_submission(
            1,
            Payload::tool_result("toolu_q", RawResult::text("no, keep them"), WireState::Raw),
            reply.submission_id,
            reply.source,
        )
        .unwrap();

    let err = message("child", "yes, delete them", Some(&seq))
        .execute(parent, RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "not delivered: child's question {seq} \"Delete the old migrations?\" was already \
             answered by you: \"no, keep them\"; to send this as a new message instead, leave \
             out reply_to"
        )
    );
    assert!(
        !child
            .snapshot()
            .iter()
            .any(|e| matches!(&e.payload, Payload::User(blocks)
                if blocks.iter().any(|b| matches!(b, ContentBlock::Text(t) if t.contains("delete them"))))),
        "a stale reply is never delivered as a turn"
    );

    fixture.host.swansong().shut_down().await;
}

/// A successor drives its predecessor: its message is a turn, not a note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successors_message_to_its_predecessor_is_a_turn() {
    let fixture = Fixture::new().await;
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());
    let said = message("parent", "what did you decide about the cache?", None)
        .execute(fixture.control("peer"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(said, "delivered: parent is working on it");
    let parent = fixture.session("parent");
    parent
        .wait_until("the predecessor logged the turn", || {
            parent
                .snapshot()
                .iter()
                .any(|entry| entry.source.as_deref() == Some("session[peer]"))
        })
        .await;

    fixture.host.swansong().shut_down().await;
}
