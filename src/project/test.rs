
use super::*;
use crate::log::{
    ContentBlock, Disposition, Keep, LogEntry, Mark, MarkKind, Notification, Origin, Payload,
    RawResult, Timestamp, ToolCall, Triage, WireState,
};
use serde_json::json;

fn entry(seq: u64, turn: u32, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn,
        at: Timestamp(1000 + seq),
        payload,
    }
}

/// A small log exercising all three Phase 0 payloads: a user turn, an
/// assistant turn with prose + a tool call, and that call's raw result.
fn sample_log() -> Vec<LogEntry> {
    vec![
        entry(
            0,
            0,
            Payload::User(vec![ContentBlock::Text("hello".into())]),
        ),
        entry(
            1,
            1,
            Payload::Assistant {
                thinking: vec![],
                text: vec![ContentBlock::Text("let me check".into())],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    // Insertion order == sorted order, so the expected-bytes
                    // assertion is robust whether serde_json is BTreeMap- or
                    // (under `preserve_order`) IndexMap-backed.
                    input: json!({ "command": "ls", "timeout": 5 }),
                }],
            },
        ),
        entry(
            2,
            1,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    content: "file.txt".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        ),
    ]
}

#[test]
fn projection_is_byte_stable() {
    let log = sample_log();
    let a = serde_json::to_string(&project(&log)).unwrap();
    let b = serde_json::to_string(&project(&log)).unwrap();
    assert_eq!(
        a, b,
        "re-projecting an identical log must yield identical bytes"
    );
}

/// Pins the exact wire encoding. If a field is reordered, renamed, or
/// added, this fails — which is the point: such a change silently breaks the
/// cache prefix in production, so we want it to break a test first.
///
/// Each tool result is prefixed with [seq] on the wire so results are
/// describable as 'reread 14' / 'branch from 14' — the entry's stable seq.
#[test]
fn projection_matches_expected_wire_bytes() {
    let wire = serde_json::to_string(&project(&sample_log())).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] file.txt"}]}]"#;
    assert_eq!(wire, expected);
}

/// Pins the exact wire encoding of a thinking-bearing assistant turn
/// (docs/transport.md): thinking leads (before text, before `tool_use`), the
/// `signature` rides verbatim, and `redacted_thinking` renders its opaque `data`. This is
/// the byte pin the thinking slice moves — reorder/rename a thinking field and it fails
/// here first, before it can silently break the cache.
#[test]
fn thinking_turn_matches_expected_wire_bytes() {
    let log = vec![
        entry(0, 1, Payload::User(vec![ContentBlock::Text("go".into())])),
        entry(
            1,
            1,
            Payload::Assistant {
                thinking: vec![
                    ContentBlock::Thinking {
                        text: "let me weigh it".into(),
                        signature: "SiG==".into(),
                    },
                    ContentBlock::RedactedThinking {
                        data: "ENC==".into(),
                    },
                ],
                text: vec![ContentBlock::Text("running it".into())],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    input: json!({ "command": "ls" }),
                }],
            },
        ),
    ];
    let wire = serde_json::to_string(&project(&log)).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"go"}]},{"role":"assistant","content":[{"type":"thinking","thinking":"let me weigh it","signature":"SiG=="},{"type":"redacted_thinking","data":"ENC=="},{"type":"text","text":"running it"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls"}}]}]"#;
    assert_eq!(wire, expected);
}

/// A `ConfigEdit` is audit-only **in every lifecycle state**: appending a `Proposed`,
/// `Committed`, or `Cancelled` entry anywhere in the log must not change a single projected
/// byte (its effect reaches the model through the classifier, not the wire). If this ever
/// fails, a config edit has leaked onto the cached prefix (docs/log-and-projection.md).
#[test]
fn config_edit_projects_to_nothing() {
    use crate::log::{ConfigChange, ConfigEdit, EditState};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();

    for state in [
        EditState::Proposed,
        EditState::Committed,
        EditState::Cancelled,
    ] {
        let mut with = sample_log();
        with.push(entry(
            3,
            1,
            Payload::ConfigEdit(ConfigEdit {
                state,
                resolves: None,
                change: ConfigChange::Command {
                    command: "cargo nextest".into(),
                },
                note: "promoted: standing-safe in scope".into(),
            }),
        ));
        let with = serde_json::to_string(&project(&with)).unwrap();
        assert_eq!(
            without, with,
            "a {state:?} config edit must not perturb the projected wire"
        );
    }
}

/// The boot preamble snapshot is replay/audit only: appending one anywhere in the log
/// must not change a single projected byte — the system prompt is carried by the
/// request's `system` field, never the `messages` array (docs/context.md).
#[test]
fn preamble_snapshot_projects_to_nothing() {
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let mut with = sample_log();
    with.push(entry(
        3,
        1,
        Payload::Preamble {
            system: "You are an agent.\n\n<CLAUDE>\nthe whole repo map\n</CLAUDE>".into(),
            context: Default::default(),
        },
    ));
    let with = serde_json::to_string(&project(&with)).unwrap();
    assert_eq!(
        without, with,
        "a preamble snapshot must not perturb the projected wire"
    );
}

/// `is_error: false` must be omitted (lean wire); a true error must appear.
#[test]
fn tool_result_omits_false_is_error_but_keeps_true() {
    let ok = serde_json::to_string(&project(&sample_log())).unwrap();
    assert!(!ok.contains("is_error"));

    let errored = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("x".into()),
                    name: "bash".into(),
                    input: json!({}),
                }],
            },
        ),
        entry(
            1,
            0,
            Payload::ToolResult {
                call_id: CallId("x".into()),
                raw: RawResult {
                    content: "boom".into(),
                    is_error: true,
                },
                wire: WireState::Raw,
            },
        ),
    ];
    assert!(
        serde_json::to_string(&project(&errored))
            .unwrap()
            .contains(r#""is_error":true"#)
    );
}

/// Results answering one assistant turn collapse into a single user message.
#[test]
fn consecutive_tool_results_coalesce_into_one_user_message() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![
                    ToolCall {
                        id: CallId("a".into()),
                        name: "bash".into(),
                        input: json!({}),
                    },
                    ToolCall {
                        id: CallId("b".into()),
                        name: "bash".into(),
                        input: json!({}),
                    },
                ],
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
            2,
            0,
            Payload::ToolResult {
                call_id: CallId("b".into()),
                raw: RawResult {
                    content: "out-b".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        ),
    ];
    let messages = project(&log);
    assert_eq!(
        messages.len(),
        2,
        "assistant turn, then ONE coalesced user turn"
    );
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(
        messages[1].content.len(),
        2,
        "both tool_results in one message"
    );
}

/// The seam-drain projection shape (docs/log-and-projection.md): a `User`
/// entry appended right after a turn's `tool_result`s projects as **one** user
/// message `[tool_result…, text]`, not two consecutive user-role messages —
/// preserving role-alternation and byte-stability. This is the shape both the
/// mid-trajectory injection and the redirect steer rely on; pinned here so a
/// change to the merge breaks a test before it can silently split the wire.
#[test]
fn user_turn_after_tool_result_merges_into_one_message() {
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
            },
        ),
        entry(
            1,
            0,
            Payload::ToolResult {
                call_id: CallId("a".into()),
                raw: RawResult {
                    content: "tool output".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        ),
        // Injected mid-trajectory (or a redirect steer): a user turn right after
        // the result, with no assistant between.
        entry(
            2,
            1,
            Payload::User(vec![ContentBlock::Text("actually, do X instead".into())]),
        ),
    ];
    let messages = project(&log);
    // assistant(tool_use) · ONE user message carrying [tool_result, text].
    assert_eq!(
        messages.len(),
        2,
        "the user turn merges into the tool_result message, not a second user message: {messages:#?}"
    );
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(
        messages[1].content.len(),
        2,
        "tool_result then text, in one user message"
    );
    assert!(
        matches!(&messages[1].content[0], ApiBlock::ToolResult { .. }),
        "tool_result leads (Anthropic requires it first): {:#?}",
        messages[1].content[0]
    );
    assert!(
        matches!(&messages[1].content[1], ApiBlock::Text { text, .. } if text == "actually, do X instead"),
        "the injected text follows: {:#?}",
        messages[1].content[1]
    );
}

/// Distillation projects the *verdict* re-rendered from `raw` (docs/redaction-fork.md): the
/// kept line and note land; the dropped lines never reach the wire.
#[test]
fn distilled_projects_the_verdict_not_raw() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("x".into()),
                    name: "bash".into(),
                    input: json!({}),
                }],
            },
        ),
        entry(
            1,
            0,
            Payload::ToolResult {
                call_id: CallId("x".into()),
                raw: RawResult {
                    content: "the gist line\nten thousand lines of output".into(),
                    is_error: false,
                },
                wire: WireState::Distilled {
                    keep: Keep::Ranges(vec![(1, 1)]),
                    note: Some("summary".into()),
                },
            },
        ),
    ];
    let wire = serde_json::to_string(&project(&log)).unwrap();
    assert!(
        wire.contains("the gist line"),
        "kept line projected: {wire}"
    );
    assert!(wire.contains("summary"), "note projected: {wire}");
    assert!(
        !wire.contains("ten thousand lines"),
        "dropped lines stay off the wire: {wire}"
    );
}

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

/// The trailing breakpoint marks exactly the last block as `ephemeral` and
/// nothing else — the cached prefix bytes are untouched, only the tail gains
/// the marker (docs/log-and-projection.md).
#[test]
fn trailing_cache_breakpoint_marks_only_the_last_block() {
    let mut messages = project(&sample_log());
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("cache_control"),
        "pure projection carries no breakpoint"
    );

    place_trailing_cache_breakpoint(&mut messages);
    let wire = serde_json::to_string(&messages).unwrap();
    assert_eq!(
        wire.matches("cache_control").count(),
        1,
        "exactly one breakpoint"
    );
    assert!(
        wire.ends_with(r#""cache_control":{"type":"ephemeral"}}]}]"#),
        "breakpoint sits on the final block: {wire}"
    );
}

/// Placement on an empty wire is a no-op, not a panic.
#[test]
fn trailing_cache_breakpoint_on_empty_is_a_noop() {
    let mut empty: Vec<ApiMessage> = Vec::new();
    place_trailing_cache_breakpoint(&mut empty);
    assert!(empty.is_empty());
}

fn notification(source: &str, body: &str) -> Payload {
    Payload::Ambient(Notification {
        source: source.into(),
        body: body.into(),
        slot: None,
    })
}

/// A pinned notification projects as a `⟢ source: body` user block **at its
/// `Triage`'s position** (docs/inbox.md) — body looked up from the `Ambient` arrival;
/// the dismissed one never projects, and the arrivals themselves project to
/// nothing.
#[test]
fn pinned_notification_projects_at_triage_position() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("t".into()),
                    name: "bash".into(),
                    input: json!({}),
                }],
            },
        ),
        entry(1, 0, notification("test-watch", "3 failures")), // pinned
        entry(2, 0, notification("reviewer", "a nit")),        // dismissed
        entry(
            3,
            0,
            Payload::ToolResult {
                call_id: CallId("t".into()),
                raw: RawResult {
                    content: "out".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        ),
        entry(
            4,
            0,
            Payload::Triage(Triage {
                for_result: Some(3),
                origin: Origin::Model,
                raw_keep: "all".into(),
                inbox: vec![(1, Disposition::Pin), (2, Disposition::Dismiss)],
                surfaced: Vec::new(),
            }),
        ),
    ];
    let messages = project(&log);
    // assistant(tool_use) · user(tool_result) · user(pinned notification).
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert_eq!(messages[0].role, Role::Assistant);
    assert_eq!(messages[1].role, Role::User); // the tool_result, flushed first
    assert_eq!(messages[2].role, Role::User); // the pin block, at the Triage position
    assert!(
        matches!(&messages[2].content[0], ApiBlock::Text { text, .. } if text == "⟢ test-watch: 3 failures")
    );
    let wire = serde_json::to_string(&messages).unwrap();
    assert!(
        !wire.contains("reviewer") && !wire.contains("a nit"),
        "dismissed off wire: {wire}"
    );
}

/// A surfaced context item projects as a `⟢ context · …` read-me reminder **at its
/// `Triage`'s position** (docs/redaction-fork.md) — the one-line description and
/// read path re-rendered from the boot `Preamble` listing, not stored on the verdict.
/// This is the byte pin the surfacing slice moves: change the render and it fails here
/// first, before it can silently break the cache. A surfaced name absent from the
/// current listing (a cross-boot change) drops silently — deterministic, never a panic.
#[test]
fn surfaced_context_item_projects_at_triage_position() {
    use crate::context::{Resolved, Section};
    let listing = Resolved {
        sections: vec![Section {
            heading: Some("Skills".into()),
            items: vec![Item::Listed {
                name: "ferritin".into(),
                description: "look up Rust docs".into(),
                path: "skills/ferritin.md".into(),
            }],
        }],
    };
    let log = vec![
        entry(
            0,
            0,
            Payload::Preamble {
                system: "sys".into(),
                context: listing,
            },
        ),
        entry(
            1,
            0,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("t".into()),
                    name: "grep".into(),
                    input: json!({}),
                }],
            },
        ),
        entry(
            2,
            0,
            Payload::ToolResult {
                call_id: CallId("t".into()),
                raw: RawResult {
                    content: "no match".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        ),
        entry(
            3,
            0,
            Payload::Triage(Triage {
                for_result: Some(2),
                origin: Origin::Model,
                raw_keep: "all".into(),
                inbox: Vec::new(),
                // "unknown" isn't in the listing → drops; only ferritin renders.
                surfaced: vec!["ferritin".into(), "unknown".into()],
            }),
        ),
    ];
    let messages = project(&log);
    // assistant(tool_use) · user(tool_result) · user(surfacing reminder). The Preamble
    // projects to nothing, so it adds no message.
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert_eq!(messages[2].role, Role::User); // the reminder, at the Triage position
    assert!(
        matches!(&messages[2].content[0], ApiBlock::Text { text, .. }
                if text == "⟢ context · ferritin may be relevant: look up Rust docs — read skills/ferritin.md"),
        "{:#?}",
        messages[2].content[0]
    );
}

/// An `Ambient` arrival with no resolving verdict (left / un-pinned) is audit-only
/// — it stays off the canonical wire.
#[test]
fn unpinned_ambient_projects_to_nothing() {
    let log = vec![
        entry(0, 0, Payload::User(vec![ContentBlock::Text("hi".into())])),
        entry(1, 0, notification("src", "left notice")),
    ];
    let messages = project(&log);
    assert_eq!(messages.len(), 1, "only the user message: {messages:#?}");
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("left notice")
    );
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// The canonical render over a stored verdict (docs/redaction-fork.md): `all` is one verbatim
/// hunk, `none` collapses to the breadcrumb note, ranges keep only their spans.
#[test]
fn render_distilled_covers_all_none_and_ranges() {
    let c = "a\nb\nc\nd\ne";
    let all = render_distilled(c, &Keep::All, None);
    assert!(all.contains("@@ lines 1-5 @@"), "{all}");
    assert!(
        all.lines().any(|l| l == "a") && all.lines().any(|l| l == "e"),
        "{all}"
    );

    let none = render_distilled(c, &Keep::None, Some("nothing useful here"));
    assert_eq!(none, "⟢ nothing useful here");

    let ranges = render_distilled(c, &Keep::Ranges(vec![(2, 3)]), None);
    assert!(ranges.contains("@@ lines 2-3 @@"), "{ranges}");
    assert!(
        ranges.lines().any(|l| l == "b") && ranges.lines().any(|l| l == "c"),
        "{ranges}"
    );
    assert!(
        !ranges.contains("@@ lines 1") && !ranges.contains("@@ line 1 "),
        "line 1 dropped: {ranges}"
    );
}

/// Past `PEEK_LINE_CAP` the render appends a truncation breadcrumb and a range
/// pointing into the truncated tail can't reach those lines (the verdict only ever
/// addressed the shown prefix).
#[test]
fn render_distilled_truncates_past_the_cap() {
    let body = (1..=PEEK_LINE_CAP + 5)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = render_distilled(&body, &Keep::Ranges(vec![(1, 1)]), None);
    assert!(out.lines().any(|l| l == "line1"), "kept line present");
    assert!(
        out.contains("5 more lines truncated"),
        "truncation noted: {out}"
    );

    let past = render_distilled(
        &body,
        &Keep::Ranges(vec![(PEEK_LINE_CAP + 2, PEEK_LINE_CAP + 4)]),
        None,
    );
    assert!(
        !past.contains(&format!("line{}", PEEK_LINE_CAP + 3)),
        "beyond-cap line unreachable: {past}"
    );
}

#[test]
fn canonical_hunk_is_verbatim_under_a_diff_header() {
    let ls = lines("alpha\nbeta\ngamma");
    let wire = render_hunks(&ls, &[(1, 3)], None);
    assert!(wire.contains("@@ lines 1-3 @@"), "{wire}");
    assert!(
        wire.lines().any(|l| l == "alpha") && wire.lines().any(|l| l == "gamma"),
        "{wire}"
    );
    assert!(
        !wire.contains("     1\talpha"),
        "no per-line numbering on the wire: {wire}"
    );
}

#[test]
fn gapped_hunks_get_a_header_each_and_drop_the_gap() {
    let body = (1..=100)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let ls = lines(&body);
    let wire = render_hunks(&ls, &[(1, 3), (99, 100)], None);
    assert!(
        wire.contains("@@ lines 1-3 @@") && wire.contains("@@ lines 99-100 @@"),
        "{wire}"
    );
    assert!(!wire.contains("line50"), "middle dropped");
    assert!(wire.len() < body.len(), "leaner than raw");
}

#[test]
fn single_line_hunk_uses_the_singular_header() {
    let ls = lines("a\nb\nc");
    let wire = render_hunks(&ls, &[(2, 2)], None);
    assert!(wire.contains("@@ line 2 @@"), "{wire}");
    assert!(wire.lines().any(|l| l == "b"));
}

#[test]
fn note_appends_and_empty_falls_back_to_breadcrumb() {
    let ls = lines("a\nb\nc");
    assert!(render_hunks(&ls, &[(2, 2)], Some("the b line")).ends_with("⟢ the b line"));
    assert_eq!(render_hunks(&ls, &[], Some("wrong file")), "⟢ wrong file");
    assert_eq!(render_hunks(&ls, &[], None), "(nothing kept)");
}

#[test]
fn normalize_clamps_sorts_and_merges() {
    assert_eq!(normalize(&[(1, 3), (2, 5)], 100), vec![(1, 5)]);
    assert_eq!(normalize(&[(1, 3), (4, 6)], 100), vec![(1, 6)]);
    assert_eq!(
        normalize(&[(310, 318), (40, 52)], 400),
        vec![(40, 52), (310, 318)]
    );
    assert_eq!(normalize(&[(1, 1000)], 100), vec![(1, 100)]);
    assert!(normalize(&[(0, 0)], 100).is_empty());
    assert!(normalize(&[(50, 60)], 10).is_empty());
}
