//! A pending `ask` as the daemon reports it (docs/supervisor.md): the state topic,
//! the status a subscriber hears, settle, and the resume a reply performs.

use super::*;
use crate::daemon::status::SessionStatus;
use crate::frontend::Approval;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

/// A reply answers a held session's question and releases the hold, as any submit does
/// (docs/control-channel.md): otherwise the operator's answer would leave the loop
/// parked at the next seam.
#[tokio::test]
async fn a_reply_resumes_a_held_session() {
    let shared = session();
    shared.set_pace(None, Some(true));
    let mut wait = shared.begin_ask("ask_1", "Proceed?".into(), 1, 1);
    assert!(shared.pace_snapshot().1, "still held while asking");

    // A hold that is waiting at the seam wakes on the reply, too.
    let seam = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { shared.pace_at_seam().await })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    shared.submit_turn(SubmittedTurn::new("yes", "sub-1"));
    assert!(!shared.pace_snapshot().1, "the reply dropped the hold");
    tokio::time::timeout(Duration::from_millis(500), seam)
        .await
        .expect("the reply woke the held seam")
        .unwrap();
    assert_eq!(wait.reply().await.unwrap().text, "yes");
}

/// The ask topic is `null` until an ask is pending, then carries the question, its place
/// among the completion's asks, and what was recalled; it is part of the composite
/// `GET /state` under the same name.
#[test]
fn the_ask_topic_carries_the_question_and_what_was_recalled() {
    let shared = session();
    let state = |shared: &Session| -> Value { serde_json::from_str(&shared.state_json()).unwrap() };
    assert_eq!(state(&shared)["ask"], Value::Null);

    shared.submit_turn(SubmittedTurn::new("earlier", "early-1"));
    let wait = shared.begin_ask("ask_1", "Which branch?".into(), 2, 3);
    assert_eq!(
        state(&shared)["ask"],
        serde_json::json!({
            "call_id": "ask_1",
            "question": "Which branch?",
            "n": 2,
            "of": 3,
            "recalled": [{"submission_id": "early-1", "text": "earlier"}],
        })
    );
    assert_eq!(state(&shared)["queued"], serde_json::json!([]));

    drop(wait);
    assert_eq!(state(&shared)["ask"], Value::Null, "nothing pending after");
}

/// The topic differ publishes the ask when it appears and clears it when it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_topic_differ_publishes_an_ask_and_its_end() {
    let shared = session();
    let mut rx = shared.subscribe_state();
    tokio::spawn(topics::watch_topics(Arc::clone(&shared)));
    let mut latest = HashMap::new();
    let mut settle = async |latest: &mut HashMap<Topic, String>| {
        while let Ok(Ok(update)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            latest.insert(update.topic, update.json.to_string());
        }
    };
    settle(&mut latest).await;
    assert_eq!(latest[&Topic::Ask], "null");

    let wait = shared.begin_ask("ask_1", "Ready?".into(), 1, 1);
    settle(&mut latest).await;
    assert!(
        latest[&Topic::Ask].contains("Ready?"),
        "{}",
        latest[&Topic::Ask]
    );

    shared.submit_turn(SubmittedTurn::new("yes", "s"));
    drop(wait);
    settle(&mut latest).await;
    assert_eq!(latest[&Topic::Ask], "null");
}

/// A question outranks a stall and the running flag, but not a held call — nothing runs
/// while a call is held. It is a decision point for `settle`, and stops being one once
/// the reply has been taken and the loop is running again.
#[test]
fn asking_is_a_status_and_a_decision_point() {
    let shared = session();
    shared.running.store(true, Ordering::SeqCst);
    assert!(!shared.is_settled());

    let wait = shared.begin_ask("ask_1", "Sure?".into(), 1, 1);
    assert_eq!(
        shared.status(),
        SessionStatus::Asking {
            question: "Sure?".into(),
            n: 1,
            of: 1
        }
    );
    assert!(shared.is_settled(), "a question is a decision point");

    let call = ToolCall::new("bash", "toolu_1", serde_json::json!({ "command": "ls" }));
    let _rx = shared.register_pending(&call, None);
    assert!(matches!(shared.status(), SessionStatus::Gated { .. }));
    assert!(shared.resolve_approval(&CallId("toolu_1".into()), Approval::Abort));

    shared.submit_turn(SubmittedTurn::new("yes", "s"));
    drop(wait);
    assert_eq!(shared.status(), SessionStatus::Running);
    assert!(!shared.is_settled(), "answered, so the loop is mid-turn");
}

/// The body a subscriber reads gives the question, its place when there are several, and
/// says a turn answers it.
#[test]
fn the_status_body_names_the_question_and_how_to_answer() {
    let shared = session();
    let status = |question: &str, n, of| SessionStatus::Asking {
        question: question.into(),
        n,
        of,
    };
    let one = shared.status_body("child", &status("Which branch?", 1, 1));
    assert!(one.starts_with("child is asking a question (with an empty log): Which branch?"));
    assert!(one.contains("sessions__turn"), "{one}");
    assert!(
        !one.contains("of 1"),
        "a lone question is not numbered: {one}"
    );

    let many = shared.status_body("child", &status("Which branch?", 2, 3));
    assert!(many.contains("question 2 of 3"), "{many}");
}

/// `continue` is declined while a question is pending, with the reason named.
#[test]
fn continue_is_declined_while_asking() {
    let shared = session();
    let _wait = shared.begin_ask("ask_1", "Sure?".into(), 1, 1);
    assert_eq!(
        shared.request_continue(),
        Err(crate::daemon::continuation::NotContinuable::Asking)
    );
}

/// An ask pushes like a turn end does: the session is waiting on a person, and the
/// message is the question. With a client attending the host the push is held for
/// the escalation delay, which is how the test observes it.
#[tokio::test]
async fn an_ask_sends_a_push_carrying_the_question() {
    let shared = session();
    let attention = Arc::new(crate::daemon::attention::Attention::default());
    attention.report_at(
        &crate::daemon::attention::AttentionReport {
            client: "c1".into(),
            viewing: None,
            active: true,
        },
        std::time::Instant::now(),
    );
    shared.set_attention(attention);
    let _wait = shared.begin_ask("ask_1", "Which branch?".into(), 1, 1);
    let message = shared.pending_escalation().expect("the ask pushed");
    assert!(message.title.starts_with("Message"), "{}", message.title);
    assert_eq!(message.body, "Which branch?");
}
