//! `project(log) -> Vec<ApiMessage>` — the deterministic view sent to the API
//! (DESIGN §1–§3).
//!
//! The whole thesis rides on this being **byte-stable**: identical input must
//! serialize to identical bytes every time, or prompt caching breaks silently.
//! The hazards (DESIGN §3) are non-deterministic map ordering (avoided by using
//! [`serde_json::Value`], which serializes object keys in sorted order) and any
//! accidental change to field order / naming on the wire (pinned by the
//! expected-bytes test below).

use serde::Serialize;
use serde_json::Value;

use crate::log::{CallId, ContentBlock, LogEntry, Payload, RawResult, WireState};

/// One message in the Anthropic `messages` array.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ApiMessage {
    pub role: Role,
    pub content: Vec<ApiBlock>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A content block on the wire. `#[serde(tag = "type")]` renders the Anthropic
/// discriminator (`"type":"text"` / `"tool_use"` / `"tool_result"`).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        // Anthropic defaults `is_error` to false; omit it when false to keep the
        // wire (and thus the cache prefix) as lean as possible.
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Fold the append-only log into the wire transcript.
///
/// Deterministic by construction: a pure function of `log`, with no clock, map
/// iteration, or other ambient input leaking into the output.
pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {
    let mut messages = Vec::new();
    // `tool_result` blocks coalesce into a single user message: Anthropic groups
    // the results answering one assistant turn's `tool_use`s into the following
    // user turn. We accumulate them and flush at the next non-tool-result entry.
    let mut pending_tool_results: Vec<ApiBlock> = Vec::new();

    for entry in log {
        match &entry.payload {
            Payload::User(blocks) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);
                messages.push(ApiMessage {
                    role: Role::User,
                    content: blocks.iter().map(block_to_api).collect(),
                });
            }
            Payload::Assistant { text, calls } => {
                flush_tool_results(&mut pending_tool_results, &mut messages);
                let mut content: Vec<ApiBlock> = text.iter().map(block_to_api).collect();
                for call in calls {
                    content.push(ApiBlock::ToolUse {
                        id: call.id.0.clone(),
                        name: call.name.clone(),
                        input: call.input.clone(),
                    });
                }
                messages.push(ApiMessage {
                    role: Role::Assistant,
                    content,
                });
            }
            Payload::ToolResult {
                call_id,
                raw,
                wire,
            } => pending_tool_results.push(tool_result_block(call_id, raw, wire)),
        }
    }
    flush_tool_results(&mut pending_tool_results, &mut messages);
    messages
}

fn flush_tool_results(pending: &mut Vec<ApiBlock>, messages: &mut Vec<ApiMessage>) {
    if !pending.is_empty() {
        messages.push(ApiMessage {
            role: Role::User,
            content: std::mem::take(pending),
        });
    }
}

fn block_to_api(block: &ContentBlock) -> ApiBlock {
    match block {
        ContentBlock::Text(text) => ApiBlock::Text { text: text.clone() },
    }
}

fn tool_result_block(call_id: &CallId, raw: &RawResult, wire: &WireState) -> ApiBlock {
    let (content, is_error) = match wire {
        WireState::Raw => (raw.content.clone(), raw.is_error),
        WireState::Distilled(distilled) => (distilled.clone(), raw.is_error),
        // Project a placeholder rather than nothing: dropping the block entirely
        // would orphan the assistant `tool_use` it answers, which the API
        // rejects. Phase 1 (which actually constructs `Elided`) must decide
        // whether elision folds the paired `tool_use` away too — flagged here.
        WireState::Elided => ("[elided]".to_string(), false),
    };
    ApiBlock::ToolResult {
        tool_use_id: call_id.0.clone(),
        content,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{ContentBlock, LogEntry, Payload, RawResult, Timestamp, ToolCall, WireState};
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
        assert_eq!(a, b, "re-projecting an identical log must yield identical bytes");
    }

    /// Pins the exact wire encoding. If a field is reordered, renamed, or
    /// added, this fails — which is the point: such a change silently breaks the
    /// cache prefix in production, so we want it to break a test first.
    #[test]
    fn projection_matches_expected_wire_bytes() {
        let wire = serde_json::to_string(&project(&sample_log())).unwrap();
        let expected = r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file.txt"}]}]"#;
        assert_eq!(wire, expected);
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
        assert!(serde_json::to_string(&project(&errored)).unwrap().contains(r#""is_error":true"#));
    }

    /// Results answering one assistant turn collapse into a single user message.
    #[test]
    fn consecutive_tool_results_coalesce_into_one_user_message() {
        let log = vec![
            entry(
                0,
                0,
                Payload::Assistant {
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
        assert_eq!(messages.len(), 2, "assistant turn, then ONE coalesced user turn");
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content.len(), 2, "both tool_results in one message");
    }

    /// Distillation projects the replacement and keeps `raw` off the wire.
    #[test]
    fn distilled_projects_replacement_not_raw() {
        let log = vec![
            entry(
                0,
                0,
                Payload::Assistant {
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
                        content: "ten thousand lines of output".into(),
                        is_error: false,
                    },
                    wire: WireState::Distilled("just the gist".into()),
                },
            ),
        ];
        let wire = serde_json::to_string(&project(&log)).unwrap();
        assert!(wire.contains("just the gist"), "distillate must be projected");
        assert!(!wire.contains("ten thousand lines"), "raw must stay off the wire");
    }
}
