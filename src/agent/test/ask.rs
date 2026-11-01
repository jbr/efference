//! Tests for the loop-owned `ask` (docs/process-monitor.md): blocking, resolution by a
//! submitted turn, recall of the turns queued before it, and the interrupt.
use super::*;
use crate::record::log::Telemetry;

use crate::frontend::SubmittedTurn;

fn session() -> Arc<Session> {
    let (shared, _rx) = Session::in_memory();
    shared
}

fn ask_call(id: &str, question: &str) -> ToolCall {
    ToolCall::new(
        crate::record::names::tools::ASK,
        id,
        serde_json::json!({ "question": question }),
    )
}

const ONE: AskPosition = AskPosition { n: 1, of: 1 };

/// Run an `ask` on its own task and wait until it is pending.
async fn start(
    shared: &Arc<Session>,
    call: ToolCall,
    at: AskPosition,
) -> tokio::task::JoinHandle<(RawResult, Option<Box<Reply>>)> {
    let task = {
        let shared = Arc::clone(shared);
        tokio::spawn(async move { ask_result(&shared, &call, at).await })
    };
    for _ in 0..200 {
        if shared.asking().is_some() {
            return task;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the ask never became pending");
}

fn queued(shared: &Session) -> Vec<String> {
    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    state["queued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["submission_id"].as_str().unwrap().to_string())
        .collect()
}

/// The next submitted turn is the reply: its text is the result, its submission and
/// source travel with it, and it never reaches the queue.
#[tokio::test(start_paused = true)]
async fn a_submitted_turn_resolves_the_ask_instead_of_queueing() {
    let shared = session();
    let task = start(&shared, ask_call("ask_1", "Which branch?"), ONE).await;
    let view = shared.asking().unwrap();
    assert_eq!(view.question, "Which branch?");
    assert_eq!((view.n, view.of), (1, 1));

    shared
        .submit_turn(SubmittedTurn::new("main", "sub-1").sourced(Some("mcp[claude-code]".into())));
    let (result, reply) = task.await.unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.content, "main");
    let reply = reply.expect("a reply");
    assert_eq!(reply.submission_id, "sub-1");
    assert_eq!(reply.source.as_deref(), Some("mcp[claude-code]"));
    assert!(shared.asking().is_none(), "nothing is pending afterwards");
    assert!(queued(&shared).is_empty(), "the reply was never queued");
    // With nothing pending, the next turn is an ordinary one.
    shared.submit_turn(SubmittedTurn::new("next", "sub-2"));
    assert_eq!(queued(&shared), vec!["sub-2"]);
}

/// A blank submission is not an answer.
#[tokio::test(start_paused = true)]
async fn a_blank_turn_does_not_resolve_the_ask() {
    let shared = session();
    let task = start(&shared, ask_call("ask_1", "Ready?"), ONE).await;
    shared.submit_turn(SubmittedTurn::new("   ", "blank"));
    assert!(shared.asking().is_some());
    shared.submit_turn(SubmittedTurn::new("yes", "real"));
    let (result, _) = task.await.unwrap();
    assert_eq!(result.content, "yes");
}

/// A notification is an ambient note, not a reply (docs/inbox.md).
#[tokio::test(start_paused = true)]
async fn a_notification_does_not_resolve_the_ask() {
    let shared = session();
    let task = start(&shared, ask_call("ask_1", "Ready?"), ONE).await;
    shared.notify(Notification::new("ci", "build finished", None));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!task.is_finished(), "still waiting");
    assert!(shared.asking().is_some());
    shared.submit_turn(SubmittedTurn::new("go", "s"));
    assert_eq!(task.await.unwrap().0.content, "go");
}

/// A turn queued before the ask started is withdrawn — not the answer, not stranded —
/// and reported on the ask in the recall route's shape.
#[tokio::test(start_paused = true)]
async fn turns_queued_before_the_ask_are_recalled_and_published() {
    let shared = session();
    shared.submit_turn(SubmittedTurn::new("earlier thought", "early-1"));
    shared.submit_turn(SubmittedTurn::new("another", "early-2"));
    assert_eq!(queued(&shared), vec!["early-1", "early-2"]);

    let task = start(&shared, ask_call("ask_1", "Which one?"), ONE).await;
    assert!(queued(&shared).is_empty(), "the queue was withdrawn");
    let recalled = serde_json::to_value(shared.asking().unwrap().recalled).unwrap();
    assert_eq!(
        recalled,
        serde_json::json!([
            {"submission_id": "early-1", "text": "earlier thought"},
            {"submission_id": "early-2", "text": "another"},
        ])
    );

    shared.submit_turn(SubmittedTurn::new("the second", "answer"));
    let (result, reply) = task.await.unwrap();
    assert_eq!(
        result.content, "the second",
        "a recalled turn is not the reply"
    );
    assert_eq!(reply.unwrap().submission_id, "answer");
    assert!(shared.asking().is_none());
}

/// An interrupt ends the ask like any tool call; nothing stays pending and the next
/// submission opens an ordinary turn.
#[tokio::test(start_paused = true)]
async fn an_interrupt_ends_the_ask_and_leaves_nothing_pending() {
    let shared = session();
    let task = start(&shared, ask_call("ask_1", "Sure?"), ONE).await;
    shared.interrupt(Default::default());
    let (result, reply) = task.await.unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("interrupted"), "{result:?}");
    assert!(reply.is_none());
    assert!(shared.asking().is_none());
    shared.submit_turn(SubmittedTurn::new("hello", "s"));
    assert_eq!(queued(&shared), vec!["s"]);

    // An interrupt already latched before the ask never registers it.
    let (result, _) = ask_result(&shared, &ask_call("ask_2", "again?"), ONE).await;
    assert!(result.content.contains("interrupted"));
    assert!(shared.asking().is_none());
}

/// A reply that lands as the ask is being interrupted is not dropped: it opens the
/// next turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_racing_an_interrupt_is_not_lost() {
    let shared = session();
    let wait = shared.begin_ask("ask_1", "q".into(), 1, 1);
    shared.submit_turn(SubmittedTurn::new("late answer", "late"));
    // The loop's claim ends before it read the reply.
    drop(wait);
    assert_eq!(queued(&shared), vec!["late"]);
}

#[tokio::test]
async fn a_missing_question_is_a_refusal_not_a_pending_ask() {
    let shared = session();
    let call = ToolCall::new(
        crate::record::names::tools::ASK,
        "ask_1",
        serde_json::json!({}),
    );
    let (result, reply) = ask_result(&shared, &call, ONE).await;
    assert!(result.is_error && result.content.contains("question"));
    assert!(reply.is_none());
    assert!(shared.asking().is_none());
}

/// Asks in one completion are numbered in call order, counting only asks.
#[test]
fn asks_in_a_batch_are_numbered_in_call_order() {
    let calls = vec![
        ask_call("a", "one"),
        bash_call("ls"),
        ask_call("b", "two"),
        ask_call("c", "three"),
    ];
    let at = |i| AskPosition::of_call(&calls, i);
    assert_eq!(at(0), AskPosition { n: 1, of: 3 });
    assert_eq!(at(2), AskPosition { n: 2, of: 3 });
    assert_eq!(at(3), AskPosition { n: 3, of: 3 });
}

/// `run_call` answers an `ask` ahead of the cascade: no gate, no spotter (there is
/// nothing to authorize), and the result carries who replied.
#[tokio::test(start_paused = true)]
async fn run_call_skips_the_cascade_and_carries_the_reply() {
    struct Silent;
    impl Frontend for Silent {
        async fn read_user_turn(&mut self) -> Option<SubmittedTurn> {
            unreachable!()
        }
        async fn approve(&mut self, call: &ToolCall, _: GateOptions<'_>) -> Approval {
            panic!("an ask reached the human gate: {}", call.name)
        }
        async fn interrupted(&mut self) -> crate::tools::Interrupt {
            unreachable!()
        }
    }
    struct NoSpotter;
    impl Spotter for NoSpotter {
        async fn assess(
            &self,
            _: &crate::config::Config,
            call: &ToolCall,
            _: Option<&crate::tools::ToolBrief<'_>>,
            _: &[String],
            _: Option<&str>,
            _: &[ToolCall],
        ) -> crate::spotter::Spotting {
            panic!("an ask reached the spotter: {}", call.name)
        }
    }
    let shared = session();
    let runner = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            run_call(
                &mut Silent,
                &shared,
                &NoSpotter,
                &Toolbox::bash_only(),
                1,
                &ask_call("ask_1", "Proceed?"),
                ONE,
                None,
            )
            .await
            .unwrap()
        })
    };
    for _ in 0..200 {
        if shared.asking().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    shared.submit_turn(SubmittedTurn::new("yes", "s1").sourced(Some("web".into())));
    match runner.await.unwrap() {
        Decision::Ran { result, reply, .. } => {
            assert_eq!(result.content, "yes");
            assert_eq!(reply.unwrap().source.as_deref(), Some("web"));
        }
        _ => panic!("an answered ask is a run result"),
    }

    // Interrupted mid-ask: the result is withheld from the wire like any tool's.
    let runner = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            run_call(
                &mut Silent,
                &shared,
                &NoSpotter,
                &Toolbox::bash_only(),
                2,
                &ask_call("ask_2", "Proceed?"),
                ONE,
                None,
            )
            .await
            .unwrap()
        })
    };
    for _ in 0..200 {
        if shared.asking().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    shared.interrupt(Default::default());
    assert!(matches!(runner.await.unwrap(), Decision::Withheld(_)));
    assert!(shared.asking().is_none());

    // An ask waits on the operator and touches nothing, so a resumed session asks it
    // again rather than dropping it: neither run left a dispatch marker.
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Telemetry(Telemetry::CallStarted { .. }))),
        "an ask is never marked started"
    );
}

/// The reply reaches the wire as the tool result, naming its question and opened with
/// the submitter's label when it has one; an operator reply (no source) adds no label,
/// and a result no submission produced (an interrupt) is not framed as a reply.
#[test]
fn the_reply_projects_as_a_result_with_the_submitters_label() {
    use crate::record::project::ApiBlock;
    let shared = session();
    let call = ask_call("ask_1", "Which?");
    shared
        .append(
            1,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call.clone()],
            )),
        )
        .unwrap();
    shared
        .append_submission(
            1,
            Payload::tool_result(
                call.id.clone(),
                RawResult::text("the second"),
                WireState::Raw,
            ),
            "sub".into(),
            Some("web".into()),
        )
        .unwrap();
    let text = |shared: &Session| match &shared.project_messages().last().unwrap().content[..] {
        [ApiBlock::ToolResult { content, .. }] => content.as_text().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(text(&shared), "[1] asked: Which?\nweb: the second");

    let plain = session();
    plain
        .append(
            1,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call.clone()],
            )),
        )
        .unwrap();
    plain
        .append_submission(
            1,
            Payload::tool_result(
                call.id.clone(),
                RawResult::text("the second"),
                WireState::Raw,
            ),
            "sub".into(),
            None,
        )
        .unwrap();
    assert_eq!(text(&plain), "[1] asked: Which?\nthe second");

    let unanswered = session();
    unanswered
        .append(
            1,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call.clone()],
            )),
        )
        .unwrap();
    unanswered
        .append(
            1,
            Payload::tool_result(
                call.id.clone(),
                RawResult::error("ask interrupted by user"),
                WireState::Raw,
            ),
        )
        .unwrap();
    assert_eq!(text(&unanswered), "[1] ask interrupted by user");
}

/// `ask` is advertised and cataloged like `wait`, with the question required.
#[test]
fn ask_is_advertised_with_a_required_question() {
    let toolbox = Toolbox::bash_only();
    let defs = tool_definitions(&toolbox);
    let ask = defs
        .iter()
        .find(|d| d["name"] == crate::record::names::tools::ASK)
        .expect("advertised");
    assert_eq!(
        ask["input_schema"]["required"],
        serde_json::json!(["question"])
    );
    assert!(
        ask["description"]
            .as_str()
            .unwrap()
            .contains("one at a time"),
        "the parallel behaviour is stated to the model"
    );
    assert!(callable_names(&toolbox).contains(&crate::record::names::tools::ASK.to_string()));
    assert!(tool_catalog(&toolbox).contains_key(crate::record::names::tools::ASK));
}
