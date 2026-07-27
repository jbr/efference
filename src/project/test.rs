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

mod cache;
mod coalesce;
mod consolidate;
mod distilled;
mod notifications;
