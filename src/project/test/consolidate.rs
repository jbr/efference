//! Summaries replacing a range of the log in the projection (docs/consolidate.md,
//! docs/summary.md).

use super::*;
use crate::log::{LoggedInference, MadeBy};

fn model() -> LoggedInference {
    LoggedInference {
        connection: "anthropic".into(),
        provider: crate::api::Provider::Anthropic,
        model: "claude-opus-5-5".into(),
    }
}

/// A head summary made by compaction: projected as written, with no harness note.
fn compacted(upto: Seq, text: &str) -> Summary {
    Summary::compaction(upto, text, model())
}

/// A [`Summary`] replaces its named range (`seq <= upto`) into a single
/// summary user message (docs/README.md). The summary message
/// lands in place of the elided entries; the live conversation after `upto`
/// projects normally.
#[test]
fn summary_replaces_its_range() {
    // Old task: user asks, model answers with a tool call and result.
    // seq 0: User("old task")
    // seq 1: Assistant(calls=[bash])
    // seq 2: ToolResult("old output")   ← upto = 2
    // seq 3: User("new task")           ← live user turn (kept)
    // seq 4: Assistant(calls=[consolidate])
    // seq 5: Summary(from:0, upto:2, text:"summary of old task")
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
                excluded: None,
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
                excluded: None,
            },
        ),
        entry(5, 3, Payload::Summary(compacted(2, "summary of old task"))),
        entry(
            6,
            3,
            Payload::tool_result("c1", RawResult::text("consolidated"), WireState::Raw),
        ),
    ];
    let messages = project(&log);

    // The old task (seqs 0-2) is gone; the summary entry (seq 5) is gone.
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

/// Multiple consolidations produce multiple summaries, each replacing their own
/// range. The session-level wire is: summary-A, task-B turns, summary-B, live
/// task-C turns.
#[test]
fn multiple_summaries_each_replace_their_own_range() {
    // Two consolidations:
    //   consolidation 1: seqs 0-1 replaced → "summary A" injected at summary seq 3
    //   consolidation 2: seqs 0-6 replaced → "summary B" injected at summary seq 8
    // After the second summary, only the live task (seq 9+) projects.
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
                excluded: None,
            },
        ),
        // First consolidation: upto=1
        entry(
            2,
            2,
            Payload::User(vec![ContentBlock::Text("task B".into())]),
        ),
        entry(3, 2, Payload::Summary(compacted(1, "summary A"))),
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
                excluded: None,
            },
        ),
        // Second consolidation: upto=4
        entry(
            5,
            4,
            Payload::User(vec![ContentBlock::Text("task C".into())]),
        ),
        entry(6, 4, Payload::Summary(compacted(4, "summary B"))),
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
                excluded: None,
            },
        ),
    ];
    let messages = project(&log);

    // Expected wire:
    //   [summary A] [summary B] [task C user] [assistant "working on C"]
    //
    // Both summaries inject just before the first live entry (seq 5, "task C"),
    // because both `upto` values (1 and 4) are exceeded by seq 5. Task B (seqs
    // 2-4) is inside the second summary's range and is elided. Summary A and
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

/// A summary at the tail of the log (no entries after it) still injects
/// the summary — degenerate edge case for an immediate consolidate.
#[test]
fn summary_at_tail_injects_text() {
    let log = vec![
        entry(0, 0, Payload::User(vec![ContentBlock::Text("old".into())])),
        entry(1, 0, Payload::Summary(compacted(0, "tail summary"))),
    ];
    let messages = project(&log);
    assert_eq!(messages.len(), 1);
    assert!(
        matches!(&messages[0].content[0], ApiBlock::Text { text, .. } if text == "tail summary")
    );
}

/// A `Summary` with `upto` covering the assistant and its first result replaces
/// them; the second result — whose `tool_use` is then gone — still projects
/// (it is outside the range) but is effectively orphaned in this contrived
/// scenario. `consolidate` only runs at clean task boundaries, so this edge
/// does not arise in practice.
#[test]
fn summary_replaces_named_range_and_injects_text() {
    // A summary with upto=1 replaces seqs 0 and 1 (assistant + first result), then
    // injects the summary, then projects the remaining entries (seq 2) normally.
    // In practice summaries only land at task boundaries; this is an explicit
    // check of the mechanical replacement behaviour.
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
                excluded: None,
            },
        ),
        entry(
            1,
            0,
            Payload::tool_result("a", RawResult::text("out-a"), WireState::Raw),
        ),
        entry(99, 0, Payload::Summary(compacted(1, "replacement text"))),
        entry(
            2,
            1,
            Payload::User(vec![ContentBlock::Text("continue".into())]),
        ),
    ];
    let messages = project(&log);
    // Seqs 0, 1 elided; summary entry elided; seq 99 injects summary; seq 2 keeps.
    // Wire: [summary user msg] [user "continue"]
    assert_eq!(messages.len(), 2, "summary + continue user: {messages:#?}");
    assert!(
        matches!(&messages[0].content[0], ApiBlock::Text { text, .. } if text == "replacement text")
    );
    assert!(matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "continue"));
}

/// A real summary over Responses replay state (docs/consolidate.md): the replaced task's
/// `function_call` / `function_call_output` units leave together, no
/// `function_call_output` replays without its call before it, and the replaced turn's
/// `encrypted_content` is gone from the wire. The live task, the summary, and the
/// consolidate exchange itself project as before.
#[test]
fn a_responses_summary_replaces_whole_replay_units() {
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
            excluded: None,
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
            Payload::Summary(Summary::consolidation(0, 2, "A is done")),
        ),
        entry(6, 1, result("consolidate_1", "consolidated")),
    ];
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
        "replaced reasoning leaves the wire: {text}"
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

/// What a summary inside a step does to the Responses wire, pinned so the construction
/// check is seen to be load-bearing: projection replaces by range and pairs nothing, so
/// a summary between a call and its result replays the output with no call before
/// it. `split_calls` names that cut and the loop refuses to append it
/// (docs/consolidate.md); a persisted summary is projected as written.
#[test]
fn a_summary_inside_a_responses_step_orphans_its_output() {
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
                excluded: None,
            },
        ),
        entry(
            2,
            0,
            Payload::tool_result("call_x", RawResult::text("out"), WireState::Raw),
        ),
        entry(3, 0, Payload::Summary(Summary::consolidation(0, 1, "cut"))),
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
        "the call was replaced, the output was not: {items:#?}"
    );
}

/// A log of two user turns around one tool call, the shape the audience tests replace over.
fn two_turn_log() -> Vec<LogEntry> {
    vec![
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
                excluded: None,
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
    ]
}

/// A summary addressed to the triage reader (docs/summary.md) projects to nothing
/// for the main reader: the main wire is byte-identical to the same log without the
/// entry, so a summary made for a separate triage client cannot move the cached prefix.
#[test]
fn a_triage_summary_leaves_the_main_projection_byte_identical() {
    let plain = two_turn_log();
    let mut with_summary = two_turn_log();
    with_summary.push(entry(
        4,
        2,
        Payload::Summary(
            Summary::consolidation(0, 2, "triage-only summary").for_audience(Audience::Triage),
        ),
    ));

    assert_eq!(
        serde_json::to_vec(&project(&with_summary)).unwrap(),
        serde_json::to_vec(&project(&plain)).unwrap(),
    );
    assert_eq!(
        format!("{:?}", project_semantic(&with_summary)),
        format!("{:?}", project_semantic(&plain)),
    );
}

/// The same summary replaces the range for the triage reader, and an `All` summary does for both.
#[test]
fn a_triage_summary_replaces_the_range_for_the_triage_reader_only() {
    let mut with_summary = two_turn_log();
    with_summary.push(entry(
        4,
        2,
        Payload::Summary(
            Summary::consolidation(0, 2, "triage-only summary").for_audience(Audience::Triage),
        ),
    ));
    let text = |turns: Vec<WireTurn>| format!("{turns:?}");

    let triage = text(project_semantic_for(&with_summary, Reader::Triage));
    assert!(triage.contains("triage-only summary"), "{triage}");
    assert!(!triage.contains("old task"), "{triage}");
    let main = text(project_semantic_for(&with_summary, Reader::Main));
    assert!(!main.contains("triage-only summary"), "{main}");
    assert!(main.contains("old task"), "{main}");

    let mut shared = two_turn_log();
    shared.push(entry(
        4,
        2,
        Payload::Summary(Summary::consolidation(0, 2, "for everyone")),
    ));
    for reader in [Reader::Main, Reader::Triage] {
        let replaced = text(project_semantic_for(&shared, reader));
        assert!(replaced.contains("for everyone"), "{reader:?}: {replaced}");
        assert!(!replaced.contains("old task"), "{reader:?}: {replaced}");
    }
}

/// A consolidation for every audience serializes without `made_by` and `audience`, and
/// reads back with the defaults (docs/summary.md); a compaction for the triage reader
/// round-trips with both, in field order `from`, `upto`, `text`, `made_by`, `audience`.
#[test]
fn made_by_and_audience_are_skipped_at_their_defaults() {
    let plain = Summary::consolidation(0, 7, "summary");
    assert_eq!(
        serde_json::to_string(&plain).unwrap(),
        r#"{"from":0,"upto":7,"text":"summary"}"#,
    );
    let old: Summary =
        serde_json::from_value(json!({"from": 0, "upto": 7, "text": "summary"})).unwrap();
    assert_eq!(old.made_by, MadeBy::Consolidation);
    assert_eq!(old.audience, Audience::All);

    let compacted = compacted(7, "account").for_audience(Audience::Triage);
    assert_eq!(
        serde_json::to_string(&compacted).unwrap(),
        r#"{"from":0,"upto":7,"text":"account","made_by":{"compaction":{"connection":"anthropic","provider":"anthropic","model":"claude-opus-5-5"}},"audience":"triage"}"#,
    );
    let back: Summary = serde_json::from_value(serde_json::to_value(&compacted).unwrap()).unwrap();
    assert_eq!(back.made_by, compacted.made_by);
    assert_eq!(back.audience, Audience::Triage);
}
