//! `Mark::Boundary` span folding (docs/consolidate.md).

use super::*;

/// A boundary [`Mark`] folds its named span (`seq <= upto`) into a single
/// summary user message (docs/README.md). The summary message
/// lands in place of the elided entries; the live conversation after `upto`
/// projects normally.
#[test]
fn boundary_mark_folds_span_to_summary() {
    // Old task: user asks, model answers with a tool call and result.
    // seq 0: User("old task")
    // seq 1: Assistant(calls=[bash])
    // seq 2: ToolResult("old output")   ← upto = 2
    // seq 3: User("new task")           ← live user turn (kept)
    // seq 4: Assistant(calls=[consolidate])
    // seq 5: Mark(Boundary{upto:2}, note:"summary of old task")
    // seq 6: ToolResult("consolidated") ← consolidate result (kept, projects normally)
    let log = vec![
        entry(
            0,
            0,
            Payload::User(vec![ContentBlock::Text("old task".into())]),
        ),
        entry(
            1,
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("bash", "t1", json!({}))],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            2,
            1,
            Payload::tool_result("t1", RawResult::text("old output"), WireState::Raw),
        ),
        entry(
            3,
            2,
            Payload::User(vec![ContentBlock::Text("new task".into())]),
        ),
        entry(
            4,
            3,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new(
                        "consolidate",
                        "c1",
                        json!({"summary": "summary of old task"}),
                    )],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            5,
            3,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 2 },
                note: "summary of old task".into(),
            }),
        ),
        entry(
            6,
            3,
            Payload::tool_result("c1", RawResult::text("consolidated"), WireState::Raw),
        ),
    ];
    let messages = project(&log);

    // The old task (seqs 0-2) is gone; the mark (seq 5) is gone.
    // Wire: [summary user msg] [user "new task"] [assistant calls=[consolidate]] [tool_result "consolidated"]
    assert_eq!(
        messages.len(),
        4,
        "summary + new-task user + consolidate assistant + result: {messages:#?}"
    );

    // First message is the injected summary.
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(
        messages[0].content,
        vec![ApiBlock::Text {
            text: "summary of old task".into(),
            cache_control: None
        }]
    );
    // Live user turn survives.
    assert_eq!(messages[1].role, Role::User);
    assert!(matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "new task"));
    // consolidate assistant + result project normally.
    assert_eq!(messages[2].role, Role::Assistant);
    assert_eq!(messages[3].role, Role::User); // tool result coalesced into user msg
}

/// Multiple consolidations produce multiple summaries, each folding their own
/// span. The session-level wire is: summary-A, task-B turns, summary-B, live
/// task-C turns.
#[test]
fn multiple_boundary_marks_each_fold_their_own_span() {
    // Two consolidations:
    //   consolidation 1: seqs 0-1 folded → "summary A" injected at mark seq 3
    //   consolidation 2: seqs 0-6 folded → "summary B" injected at mark seq 8
    // After the second fold, only the live task (seq 9+) projects.
    let log = vec![
        // First task
        entry(
            0,
            0,
            Payload::User(vec![ContentBlock::Text("task A".into())]),
        ),
        entry(
            1,
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text("done A".into())],
                    vec![],
                ),
                upstream_failure: None,
            },
        ),
        // First consolidation: upto=1
        entry(
            2,
            2,
            Payload::User(vec![ContentBlock::Text("task B".into())]),
        ),
        entry(
            3,
            2,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 1 },
                note: "summary A".into(),
            }),
        ),
        // Task B turns
        entry(
            4,
            3,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text("working on B".into())],
                    vec![],
                ),
                upstream_failure: None,
            },
        ),
        // Second consolidation: upto=4
        entry(
            5,
            4,
            Payload::User(vec![ContentBlock::Text("task C".into())]),
        ),
        entry(
            6,
            4,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 4 },
                note: "summary B".into(),
            }),
        ),
        // Live task C
        entry(
            7,
            5,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text("working on C".into())],
                    vec![],
                ),
                upstream_failure: None,
            },
        ),
    ];
    let messages = project(&log);

    // Expected wire:
    //   [summary A] [summary B] [task C user] [assistant "working on C"]
    //
    // Both summaries inject just before the first live entry (seq 5, "task C"),
    // because both `upto` values (1 and 4) are exceeded by seq 5. Task B (seqs
    // 2-4) is inside the second fold's span and is elided. Summary A and
    // summary B together give the model the full session history.
    assert_eq!(
        messages.len(),
        4,
        "two summaries + task-C turns: {messages:#?}"
    );
    assert!(matches!(&messages[0].content[0], ApiBlock::Text { text, .. } if text == "summary A"));
    assert!(matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "summary B"));
    assert!(matches!(&messages[2].content[0], ApiBlock::Text { text, .. } if text == "task C"));
    assert!(
        matches!(&messages[3].content[0], ApiBlock::Text { text, .. } if text == "working on C")
    );
}

/// A boundary mark at the tail of the log (no entries after it) still injects
/// the summary — degenerate edge case for an immediate consolidate.
#[test]
fn boundary_mark_at_tail_injects_summary() {
    let log = vec![
        entry(0, 0, Payload::User(vec![ContentBlock::Text("old".into())])),
        entry(
            1,
            0,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 0 },
                note: "tail summary".into(),
            }),
        ),
    ];
    let messages = project(&log);
    assert_eq!(messages.len(), 1);
    assert!(
        matches!(&messages[0].content[0], ApiBlock::Text { text, .. } if text == "tail summary")
    );
}

/// A `Mark` with `upto` covering the assistant and its first result *folds*
/// them; the second result — whose `tool_use` is then gone — still projects
/// (it is outside the fold) but is effectively orphaned in this contrived
/// scenario. `consolidate` only runs at clean task boundaries, so this edge
/// does not arise in practice.
#[test]
fn mark_folds_named_span_and_injects_summary() {
    // A mark with upto=1 folds seqs 0 and 1 (assistant + first result), then
    // injects the summary, then projects the remaining entries (seq 2) normally.
    // In practice marks only land at task boundaries; this is an explicit
    // check of the mechanical fold behaviour.
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("bash", "a", json!({}))],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            1,
            0,
            Payload::tool_result("a", RawResult::text("out-a"), WireState::Raw),
        ),
        entry(
            99,
            0,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 1 },
                note: "folded note".into(),
            }),
        ),
        entry(
            2,
            1,
            Payload::User(vec![ContentBlock::Text("continue".into())]),
        ),
    ];
    let messages = project(&log);
    // Seqs 0, 1 elided; mark elided; seq 99 injects summary; seq 2 keeps.
    // Wire: [summary user msg] [user "continue"]
    assert_eq!(messages.len(), 2, "summary + continue user: {messages:#?}");
    assert!(
        matches!(&messages[0].content[0], ApiBlock::Text { text, .. } if text == "folded note")
    );
    assert!(matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "continue"));
}

/// A real boundary over Responses replay state (docs/consolidate.md): the folded task's
/// `function_call` / `function_call_output` units leave together, no
/// `function_call_output` replays without its call before it, and the folded turn's
/// `encrypted_content` is gone from the wire. The live task, the summary, and the
/// consolidate exchange itself project as before.
#[test]
fn a_responses_boundary_folds_whole_replay_units() {
    use crate::api::{PromptCachePolicy, encode_responses};
    use crate::log::ResponsesOutputItem;

    fn call(call_id: &str, encrypted: &str) -> Payload {
        Payload::Assistant {
            output: AssistantOutput::Responses(vec![
                ResponsesOutputItem::Reasoning {
                    id: format!("rs_{call_id}"),
                    status: None,
                    summary: Vec::new(),
                    content: Vec::new(),
                    encrypted_content: Some(encrypted.into()),
                },
                ResponsesOutputItem::FunctionCall {
                    id: None,
                    call_id: CallId(call_id.into()),
                    name: "bash".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                    status: None,
                },
            ]),
            upstream_failure: None,
        }
    }
    fn result(call_id: &str, out: &str) -> Payload {
        Payload::tool_result(call_id, RawResult::text(out), WireState::Raw)
    }
    let log = vec![
        entry(
            0,
            0,
            Payload::User(vec![ContentBlock::Text("task A".into())]),
        ),
        entry(1, 0, call("call_a", "ENC-A")),
        entry(2, 0, result("call_a", "out-a")),
        entry(
            3,
            1,
            Payload::User(vec![ContentBlock::Text("task B".into())]),
        ),
        entry(4, 1, call("consolidate_1", "ENC-C")),
        entry(
            5,
            1,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary {
                    upto: crate::consolidate::extent(&log_prefix()),
                },
                note: "A is done".into(),
            }),
        ),
        entry(6, 1, result("consolidate_1", "consolidated")),
    ];
    // `extent` reads the log as it stood when the consolidate call was made.
    fn log_prefix() -> Vec<LogEntry> {
        vec![
            entry(
                0,
                0,
                Payload::User(vec![ContentBlock::Text("task A".into())]),
            ),
            entry(1, 0, call("call_a", "ENC-A")),
            entry(2, 0, result("call_a", "out-a")),
            entry(
                3,
                1,
                Payload::User(vec![ContentBlock::Text("task B".into())]),
            ),
            entry(4, 1, call("consolidate_1", "ENC-C")),
        ]
    }
    assert_eq!(crate::consolidate::extent(&log_prefix()), 2);
    assert!(crate::consolidate::split_calls(&log, 2).is_empty());

    let input = encode_responses(&project_semantic(&log), "", PromptCachePolicy::None);
    let items: Vec<serde_json::Value> = input
        .iter()
        .map(|i| serde_json::to_value(i).unwrap())
        .collect();
    let text = serde_json::to_string(&items).unwrap();
    assert!(
        !text.contains("out-a") && !text.contains("call_a"),
        "{text}"
    );
    assert!(
        !text.contains("ENC-A"),
        "folded reasoning leaves the wire: {text}"
    );
    assert!(
        text.contains("A is done") && text.contains("task B"),
        "{text}"
    );
    assert!(
        text.contains("ENC-C"),
        "the live turn's reasoning replays: {text}"
    );
    assert!(!text.contains("task A"), "{text}");
    // Every output has its call before it.
    for (i, item) in items.iter().enumerate() {
        if item["type"] == "function_call_output" {
            let call_id = &item["call_id"];
            assert!(
                items[..i]
                    .iter()
                    .any(|c| c["type"] == "function_call" && &c["call_id"] == call_id),
                "orphan output {call_id}: {items:#?}"
            );
        }
    }
}

/// What a mark inside a step does to the Responses wire, pinned so the construction
/// check is seen to be load-bearing: projection folds by range and pairs nothing, so
/// a boundary between a call and its result replays the output with no call before
/// it. `split_calls` names that boundary and the loop refuses to append it
/// (docs/consolidate.md); a persisted mark is projected as written.
#[test]
fn a_boundary_inside_a_responses_step_orphans_its_output() {
    use crate::api::{PromptCachePolicy, encode_responses};
    use crate::log::ResponsesOutputItem;
    let log = vec![
        entry(0, 0, Payload::User(vec![ContentBlock::Text("go".into())])),
        entry(
            1,
            0,
            Payload::Assistant {
                output: AssistantOutput::Responses(vec![ResponsesOutputItem::FunctionCall {
                    id: None,
                    call_id: CallId("call_x".into()),
                    name: "bash".into(),
                    arguments: "{}".into(),
                    status: None,
                }]),
                upstream_failure: None,
            },
        ),
        entry(
            2,
            0,
            Payload::tool_result("call_x", RawResult::text("out"), WireState::Raw),
        ),
        entry(
            3,
            0,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 1 },
                note: "cut".into(),
            }),
        ),
        entry(4, 1, Payload::User(vec![ContentBlock::Text("on".into())])),
    ];
    assert_eq!(
        crate::consolidate::split_calls(&log, 1),
        vec![CallId("call_x".into())]
    );
    let input = encode_responses(&project_semantic(&log), "", PromptCachePolicy::None);
    let items: Vec<serde_json::Value> = input
        .iter()
        .map(|i| serde_json::to_value(i).unwrap())
        .collect();
    assert!(
        items.iter().any(|i| i["type"] == "function_call_output"),
        "{items:#?}"
    );
    assert!(
        !items.iter().any(|i| i["type"] == "function_call"),
        "the call folded, the output did not: {items:#?}"
    );
}
