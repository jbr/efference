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

use std::collections::HashSet;

use crate::log::{
    CallId, ContentBlock, LogEntry, Mark, MarkKind, Payload, RawResult, Seq, WireState,
};

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

/// A prompt-cache breakpoint marker (DESIGN §3). `ephemeral` is the 5-minute TTL.
/// Serializes as `{"type":"ephemeral"}`. It rides on whichever content block it's
/// attached to; [`place_trailing_cache_breakpoint`] puts exactly one at the wire
/// tail each request, so the (stable) prefix before it is cache-read while only
/// the small new tail is cache-written.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CacheControl {
    Ephemeral,
}

/// A content block on the wire. `#[serde(tag = "type")]` renders the Anthropic
/// discriminator (`"type":"text"` / `"tool_use"` / `"tool_result"`).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        // Anthropic defaults `is_error` to false; omit it when false to keep the
        // wire (and thus the cache prefix) as lean as possible.
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

impl ApiBlock {
    /// Set (or clear) this block's cache breakpoint, whatever its variant.
    fn set_cache_control(&mut self, cc: Option<CacheControl>) {
        match self {
            ApiBlock::Text { cache_control, .. }
            | ApiBlock::ToolUse { cache_control, .. }
            | ApiBlock::ToolResult { cache_control, .. } => *cache_control = cc,
        }
    }
}

/// Place the single trailing cache breakpoint (DESIGN §3, Phase 0): mark the last
/// block of the last message `ephemeral`. Kept out of [`project`] so projection
/// stays a pure, byte-stable function (the breakpoint moves every request; the
/// prefix it caches does not). Idempotent re-placement on an identical wire is
/// byte-identical.
pub fn place_trailing_cache_breakpoint(messages: &mut [ApiMessage]) {
    if let Some(block) = messages.last_mut().and_then(|m| m.content.last_mut()) {
        block.set_cache_control(Some(CacheControl::Ephemeral));
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A boundary mark and the summary it contributes, extracted in the pre-pass.
struct Boundary {
    /// The `upto` that the mark named — the last seq of the elided old-task span.
    /// The summary is injected just before the first non-elided entry with
    /// `seq > upto` (i.e., the start of the live conversation that follows).
    upto: Seq,
    /// The one-line summary the model wrote at consolidation time.
    summary: String,
}

/// Pre-pass: collect boundary marks so the main loop can elide their spans.
///
/// Returns `(boundaries, elided_seqs)`:
/// - `boundaries` — one entry per [`MarkKind::Boundary`], in log order.
/// - `elided_seqs` — the set of [`Seq`]s to skip in the main loop; for each
///   boundary this is every entry with `seq <= upto` plus the mark itself.
///
/// When multiple boundaries exist the folds may overlap (each later
/// consolidation subsumes the earlier ones). The later boundary's summary
/// wins for any seq covered by both folds.
fn extract_boundaries(log: &[LogEntry]) -> (Vec<Boundary>, HashSet<Seq>) {
    let mut boundaries = Vec::new();
    let mut elided = HashSet::new();
    for entry in log {
        if let Payload::Mark(Mark {
            kind: MarkKind::Boundary { upto },
            note,
        }) = &entry.payload
        {
            let upto = *upto;
            // Elide the old-task span and the mark itself.
            for e in log {
                if e.seq <= upto || e.seq == entry.seq {
                    elided.insert(e.seq);
                }
            }
            boundaries.push(Boundary {
                upto,
                summary: note.clone(),
            });
        }
    }
    (boundaries, elided)
}

/// Fold the append-only log into the wire transcript.
///
/// Deterministic by construction: a pure function of `log`, with no clock, map
/// iteration, or other ambient input leaking into the output.
///
/// When the log contains a [`MarkKind::Boundary`], the span it names (`seq <=
/// upto`) is elided and replaced by a single user-role summary message injected
/// at the mark's position. The live conversation after `upto` — and the
/// consolidate `tool_use` / result that recorded the boundary — project normally
/// (DESIGN §10 Phase 3 Slice 1).
pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {
    let (boundaries, elided) = extract_boundaries(log);
    // Cursor into `boundaries` — advance past each one as we emit the first
    // non-elided entry after its `upto`.
    let mut boundary_cursor = 0;

    let mut messages = Vec::new();
    // `tool_result` blocks coalesce into a single user message: Anthropic groups
    // the results answering one assistant turn's `tool_use`s into the following
    // user turn. We accumulate them and flush at the next non-tool-result entry.
    let mut pending_tool_results: Vec<ApiBlock> = Vec::new();

    for entry in log {
        // Inject the summary just before the first non-elided entry that follows
        // the fold span (i.e., the first entry with seq > upto). This places the
        // summary at the head of the live conversation rather than at the mark's
        // own log position (DESIGN §10 Phase 3 Slice 1).
        while boundary_cursor < boundaries.len()
            && !elided.contains(&entry.seq)
            && entry.seq > boundaries[boundary_cursor].upto
        {
            let b = &boundaries[boundary_cursor];
            flush_tool_results(&mut pending_tool_results, &mut messages);
            messages.push(ApiMessage {
                role: Role::User,
                content: vec![ApiBlock::Text {
                    text: b.summary.clone(),
                    cache_control: None,
                }],
            });
            boundary_cursor += 1;
        }

        // Skip elided entries (the folded old-task span + the mark itself).
        if elided.contains(&entry.seq) {
            continue;
        }

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
                        cache_control: None,
                    });
                }
                messages.push(ApiMessage {
                    role: Role::Assistant,
                    content,
                });
            }
            Payload::ToolResult { call_id, raw, wire } => {
                pending_tool_results.push(tool_result_block(entry.seq, call_id, raw, wire))
            }
            // Marks are in the elided set (extract_boundaries inserts every
            // Boundary mark's own seq); this arm is only reachable for future
            // non-Boundary mark kinds, which can be added here when they land.
            Payload::Mark(_) => {}
        }
    }

    // Flush any boundaries whose upto is past the end of the log (degenerate
    // edge: consolidate called with nothing after it yet).

    while boundary_cursor < boundaries.len() {
        let b = &boundaries[boundary_cursor];
        flush_tool_results(&mut pending_tool_results, &mut messages);
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![ApiBlock::Text {
                text: b.summary.clone(),
                cache_control: None,
            }],
        });
        boundary_cursor += 1;
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
        ContentBlock::Text(text) => ApiBlock::Text {
            text: text.clone(),
            cache_control: None,
        },
    }
}

fn tool_result_block(seq: u64, call_id: &CallId, raw: &RawResult, wire: &WireState) -> ApiBlock {
    let (content, is_error) = match wire {
        WireState::Raw => (raw.content.clone(), raw.is_error),
        WireState::Distilled(distilled) => (distilled.clone(), raw.is_error),
        // Project a placeholder rather than nothing: dropping the block entirely
        // would orphan the assistant `tool_use` it answers, which the API
        // rejects. Phase 1 (which actually constructs `Elided`) must decide
        // whether elision folds the paired `tool_use` away too — flagged here.
        WireState::Elided => ("[elided]".to_string(), false),
    };
    // Prefix with [seq] handle so tool results are describable as 'reread 14' /
    // 'branch from 14' — the entry's stable seq. The wire never carries seq
    // itself; this handle makes it visible at read-time without schema change
    // (DESIGN §10 Phase 1 slice 2b).
    let prefixed = format!("[{}] {}", seq, content);
    ApiBlock::ToolResult {
        tool_use_id: call_id.0.clone(),
        content: prefixed,
        is_error,
        cache_control: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{
        ContentBlock, LogEntry, Mark, MarkKind, Payload, RawResult, Timestamp, ToolCall, WireState,
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
        assert!(
            wire.contains("just the gist"),
            "distillate must be projected"
        );
        assert!(
            !wire.contains("ten thousand lines"),
            "raw must stay off the wire"
        );
    }

    /// A boundary [`Mark`] folds its named span (`seq <= upto`) into a single
    /// summary user message (DESIGN §10 Phase 3 Slice 1). The summary message
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
        assert!(
            matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "new task")
        );
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
        assert!(
            matches!(&messages[0].content[0], ApiBlock::Text { text, .. } if text == "summary A")
        );
        assert!(
            matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "summary B")
        );
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
        assert!(
            matches!(&messages[1].content[0], ApiBlock::Text { text, .. } if text == "continue")
        );
    }

    /// The trailing breakpoint marks exactly the last block as `ephemeral` and
    /// nothing else — the cached prefix bytes are untouched, only the tail gains
    /// the marker (DESIGN §3).
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
}
