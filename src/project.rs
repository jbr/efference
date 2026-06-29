//! `project(log) -> Vec<ApiMessage>` — the deterministic view sent to the API
//! (docs/README.md, docs/substrate.md).
//!
//! The whole thesis rides on this being **byte-stable**: identical input must
//! serialize to identical bytes every time, or prompt caching breaks silently.
//! The hazards (docs/substrate.md) are non-deterministic map ordering (avoided by
//! using [`serde_json::Value`], which serializes object keys in sorted order) and
//! any accidental change to field order / naming on the wire (pinned by the
//! expected-bytes test below).

use serde::Serialize;
use serde_json::Value;

use std::collections::{HashMap, HashSet};

use crate::log::{
    CallId, ContentBlock, Disposition, Keep, LogEntry, Mark, MarkKind, Notification, Payload,
    RawResult, Seq, WireState,
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

/// A prompt-cache breakpoint marker (docs/substrate.md). `ephemeral` is the 5-minute TTL.
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

/// Place the single trailing cache breakpoint (docs/substrate.md): mark the last
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
/// (docs/README.md).
pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {
    let (boundaries, elided) = extract_boundaries(log);
    // Notification bodies by arrival `seq`, so a `Triage` can emit a block for each
    // notification it pinned (docs/inbox.md) without re-walking the log.
    let notifications: HashMap<Seq, &Notification> = log
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Ambient(n) => Some((e.seq, n)),
            _ => None,
        })
        .collect();
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
        // own log position (docs/README.md).
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
            // An `Ambient` arrival is audit-only on the wire (docs/inbox.md) — it never
            // itself projects; a *pinned* one is emitted by its `Triage` (below).
            Payload::Ambient(_) => {}
            // A spotter verdict (docs/permissions.md) is audit-only — the permission
            // decision lives in the log, never on the canonical wire.
            Payload::Spot(_) => {}
            // A grant (backlog/permissions/config-edit.md) is audit-only too — its
            // *effect* reaches the model through the classifier (the policy fold), never
            // the transcript. So it projects to nothing, and appending one cannot perturb
            // the cached prefix (docs/substrate.md).
            Payload::ConfigEdit(_) => {}
            // A fork verdict emits a block per **pinned** notification at its own
            // position (docs/inbox.md) — body looked up from the `Ambient` arrival. This
            // tail-ward placement keeps the wire append-only: a notification pinned at
            // a later gate than its first-show never splices behind a breakpoint \(docs/substrate.md\).
            // Dismissed / left dispositions don't project. The verdict is otherwise
            // audit-only.
            Payload::Triage(t) => {
                let pins: Vec<String> = t
                    .inbox
                    .iter()
                    .filter(|(_, d)| matches!(d, Disposition::Pin))
                    .filter_map(|(seq, _)| {
                        notifications
                            .get(seq)
                            .map(|n| format!("⟢ {}: {}", n.source, n.body))
                    })
                    .collect();
                if !pins.is_empty() {
                    flush_tool_results(&mut pending_tool_results, &mut messages);
                    messages.push(ApiMessage {
                        role: Role::User,
                        content: vec![ApiBlock::Text {
                            text: pins.join("\n"),
                            cache_control: None,
                        }],
                    });
                }
            }
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
        // Re-render the distillate from `raw` + the stored verdict each turn
        // (docs/redaction-fork.md) — the kept bytes are never persisted twice.
        WireState::Distilled { keep, note } => {
            (render_distilled(&raw.content, keep, note.as_deref()), raw.is_error)
        }
        // Project a placeholder rather than nothing: dropping the block entirely
        // would orphan the assistant `tool_use` it answers, which the API
        // rejects. Phase 1 (which actually constructs `Elided`) must decide
        // whether elision folds the paired `tool_use` away too — flagged here.
        WireState::Elided => ("[elided]".to_string(), false),
    };
    // Prefix with [seq] handle so tool results are describable as 'reread 14' /
    // 'branch from 14' — the entry's stable seq. The wire never carries seq
    // itself; this handle makes it visible at read-time without schema change
    // (docs/README.md).
    let prefixed = format!("[{}] {}", seq, content);
    ApiBlock::ToolResult {
        tool_use_id: call_id.0.clone(),
        content: prefixed,
        is_error,
        cache_control: None,
    }
}

/// Hard cap on the lines the fork peek shows the model (docs/redaction-fork.md). The canonical
/// render interprets a verdict's ranges against this same prefix of `raw`, so the two
/// stay aligned — the model's `retain(lines=…)` coordinates address exactly what it saw.
pub const PEEK_LINE_CAP: usize = 2000;

/// Render a distilled tool result for the canonical wire from `raw` + the stored
/// verdict (docs/redaction-fork.md): the kept ranges as diff-style `@@`-hunks over the verbatim
/// lines, the optional `note`, and a truncation breadcrumb past the cap. The wire
/// projection and the TUI projection share this one renderer. **Deterministic** — part
/// of the byte-stability contract (docs/substrate.md): a pure function of its inputs.
pub fn render_distilled(content: &str, keep: &Keep, note: Option<&str>) -> String {
    let all: Vec<&str> = content.lines().collect();
    let total = all.len();
    let shown = &all[..total.min(PEEK_LINE_CAP)];
    let dropped = total - shown.len();
    let ranges: Vec<(usize, usize)> = match keep {
        Keep::All => vec![(1, shown.len())],
        Keep::None => vec![],
        Keep::Ranges(rs) => rs.clone(),
    };
    let mut out = render_hunks(shown, &ranges, note);
    if dropped > 0 {
        out.push('\n');
        out.push_str(&format!("… [{dropped} more lines truncated before triage]"));
    }
    out
}

/// Render kept ranges as diff-style hunks: a `@@ lines a-b @@` header per contiguous
/// span (`@@ line n @@` for one line), then those lines *verbatim* (unnumbered). The
/// header carries the position anchor at one line per *range*; gaps between hunks are
/// implied by the jump in the headers, as in a unified diff. An optional trailing note
/// is the breadcrumb kept even when nothing is retained (docs/redaction-fork.md) — one
/// overall note, so it survives the empty-hunks case.
fn render_hunks(lines: &[&str], ranges: &[(usize, usize)], note: Option<&str>) -> String {
    let ranges = normalize(ranges, lines.len());
    let mut parts: Vec<String> = Vec::new();
    for &(start, end) in &ranges {
        parts.push(hunk_header(start, end));
        for i in start..=end {
            parts.push(lines[i - 1].to_string());
        }
    }
    if let Some(note) = note.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("— {note}"));
    }
    if parts.is_empty() {
        parts.push("(nothing kept)".to_string());
    }
    parts.join("\n")
}

/// A unified-diff-style hunk header: `@@ lines a-b @@`, or `@@ line n @@` for a single
/// kept line.
fn hunk_header(start: usize, end: usize) -> String {
    if start == end {
        format!("@@ line {start} @@")
    } else {
        format!("@@ lines {start}-{end} @@")
    }
}

/// Clamp ranges to `[1, n]`, drop out-of-range, sort, and merge overlapping/adjacent.
/// Tolerates reversed ranges.
fn normalize(ranges: &[(usize, usize)], n: usize) -> Vec<(usize, usize)> {
    if n == 0 {
        return Vec::new();
    }
    let mut rs: Vec<(usize, usize)> = ranges
        .iter()
        .map(|&(s, e)| (s.min(e), s.max(e)))
        .filter(|&(s, _)| s >= 1 && s <= n) // drop wholly-out-of-range
        .map(|(s, e)| (s, e.min(n))) // clamp the tail down to n
        .collect();
    rs.sort_by_key(|&(s, _)| s);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in rs {
        match merged.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
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

    /// A `ConfigEdit` is audit-only: appending one anywhere in the log must not change a single
    /// projected byte (its effect reaches the model through the classifier, not the wire). If
    /// this ever fails, a grant has leaked onto the cached prefix (docs/substrate.md).
    #[test]
    fn grant_projects_to_nothing() {
        use crate::log::{Amendment, ConfigEdit, Lifetime};
        let without = serde_json::to_string(&project(&sample_log())).unwrap();

        let mut with = sample_log();
        with.push(entry(
            3,
            1,
            Payload::ConfigEdit(ConfigEdit {
                amendment: Amendment::Allow {
                    program: "cargo".into(),
                    prefix: vec!["nextest".into()],
                },
                lifetime: Lifetime::Persist,
                note: "promoted: standing-safe in scope".into(),
            }),
        ));
        let with = serde_json::to_string(&project(&with)).unwrap();

        assert_eq!(without, with, "a grant must not perturb the projected wire");
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

    /// Distillation projects the *verdict* re-rendered from `raw` (docs/redaction-fork.md): the
    /// kept line and note land; the dropped lines never reach the wire.
    #[test]
    fn distilled_projects_the_verdict_not_raw() {
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
        assert!(wire.contains("the gist line"), "kept line projected: {wire}");
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
    /// the marker (docs/substrate.md).
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
        Payload::Ambient(Notification { source: source.into(), body: body.into(), slot: None })
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
                    raw: RawResult { content: "out".into(), is_error: false },
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
        assert!(!wire.contains("reviewer") && !wire.contains("a nit"), "dismissed off wire: {wire}");
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
        assert!(!serde_json::to_string(&messages).unwrap().contains("left notice"));
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
        assert!(all.lines().any(|l| l == "a") && all.lines().any(|l| l == "e"), "{all}");

        let none = render_distilled(c, &Keep::None, Some("nothing useful here"));
        assert_eq!(none, "— nothing useful here");

        let ranges = render_distilled(c, &Keep::Ranges(vec![(2, 3)]), None);
        assert!(ranges.contains("@@ lines 2-3 @@"), "{ranges}");
        assert!(ranges.lines().any(|l| l == "b") && ranges.lines().any(|l| l == "c"), "{ranges}");
        assert!(!ranges.contains("@@ lines 1") && !ranges.contains("@@ line 1 "), "line 1 dropped: {ranges}");
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
        assert!(out.contains("5 more lines truncated"), "truncation noted: {out}");

        let past =
            render_distilled(&body, &Keep::Ranges(vec![(PEEK_LINE_CAP + 2, PEEK_LINE_CAP + 4)]), None);
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
        assert!(wire.lines().any(|l| l == "alpha") && wire.lines().any(|l| l == "gamma"), "{wire}");
        assert!(!wire.contains("     1\talpha"), "no per-line numbering on the wire: {wire}");
    }

    #[test]
    fn gapped_hunks_get_a_header_each_and_drop_the_gap() {
        let body = (1..=100).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let ls = lines(&body);
        let wire = render_hunks(&ls, &[(1, 3), (99, 100)], None);
        assert!(wire.contains("@@ lines 1-3 @@") && wire.contains("@@ lines 99-100 @@"), "{wire}");
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
        assert!(render_hunks(&ls, &[(2, 2)], Some("the b line")).ends_with("— the b line"));
        assert_eq!(render_hunks(&ls, &[], Some("wrong file")), "— wrong file");
        assert_eq!(render_hunks(&ls, &[], None), "(nothing kept)");
    }

    #[test]
    fn normalize_clamps_sorts_and_merges() {
        assert_eq!(normalize(&[(1, 3), (2, 5)], 100), vec![(1, 5)]);
        assert_eq!(normalize(&[(1, 3), (4, 6)], 100), vec![(1, 6)]);
        assert_eq!(normalize(&[(310, 318), (40, 52)], 400), vec![(40, 52), (310, 318)]);
        assert_eq!(normalize(&[(1, 1000)], 100), vec![(1, 100)]);
        assert!(normalize(&[(0, 0)], 100).is_empty());
        assert!(normalize(&[(50, 60)], 10).is_empty());
    }
}
