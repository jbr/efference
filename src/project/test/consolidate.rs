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
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("t1".into()),
                    name: "bash".into(),
                    input: json!({}),
                }],
                upstream_failure: None,
            },
        ),
        entry(
            2,
            1,
            Payload::ToolResult {
                call_id: CallId("t1".into()),
                raw: RawResult {
                    content: "old output".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
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
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("c1".into()),
                    name: "consolidate".into(),
                    input: json!({"summary": "summary of old task"}),
                }],
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
            Payload::ToolResult {
                call_id: CallId("c1".into()),
                raw: RawResult {
                    content: "consolidated".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
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
                thinking: vec![],
                text: vec![ContentBlock::Text("done A".into())],
                calls: vec![],
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
                thinking: vec![],
                text: vec![ContentBlock::Text("working on B".into())],
                calls: vec![],
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
                thinking: vec![],
                text: vec![ContentBlock::Text("working on C".into())],
                calls: vec![],
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

/// Slice 0 test updated: a `Mark` with `upto` covering the assistant and its
/// first result *folds* them; the second result — whose `tool_use` is now gone
/// — still projects (it's outside the fold) but is effectively orphaned in
/// this contrived scenario. In practice `consolidate` only runs at clean task
/// boundaries so this edge never occurs.
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
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("a".into()),
                    name: "bash".into(),
                    input: json!({}),
                }],
                upstream_failure: None,
            },
        ),
        entry(
            1,
            0,
            Payload::ToolResult {
                call_id: CallId("a".into()),
                raw: RawResult {
                    content: "out-a".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
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
