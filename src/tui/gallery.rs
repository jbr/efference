//! The element gallery — a fixed "one of each" transcript for eyeballing the real
//! TUI (`efference gallery`), the terminal cousin of a component design page.
//!
//! It builds a [`LogEntry`] sequence that exercises every transcript shape — rich
//! markdown prose (headings, lists, quote, table, code, rule, inline styles), a
//! `bash` call rendered as a shell block, an edit tool's colored diff, a long
//! result collapsed under the disclosure triangle, an error result, a distilled
//! result, and a consolidation mark — then runs the real [`ui_loop`](super::ui_loop)
//! against it with no daemon behind it. The snapshot harness (`harness.rs`) covers
//! the same shapes headlessly; this is for seeing them live, in color, scrollable.

use crate::log::{
    CallId, ContentBlock, LogEntry, Mark, MarkKind, Payload, RawResult, Timestamp, ToolCall,
    WireState,
};

/// A committed user turn carrying markdown prose.
fn user(seq: u64, text: &str) -> LogEntry {
    entry(seq, Payload::User(vec![ContentBlock::Text(text.into())]))
}

/// A committed assistant turn: markdown prose plus zero or more tool calls.
fn assistant(seq: u64, text: &str, calls: Vec<ToolCall>) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            text: vec![ContentBlock::Text(text.into())],
            calls,
        },
    )
}

/// A committed tool result for `call_id`, at the given wire state.
fn result(seq: u64, call_id: &str, content: &str, is_error: bool, wire: WireState) -> LogEntry {
    entry(
        seq,
        Payload::ToolResult {
            call_id: CallId(call_id.into()),
            raw: RawResult { content: content.into(), is_error },
            wire,
        },
    )
}

fn call(name: &str, call_id: &str, input: serde_json::Value) -> ToolCall {
    ToolCall { id: CallId(call_id.into()), name: name.into(), input }
}

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry { seq, turn: 1, at: Timestamp(0), payload }
}

/// Markdown prose exercising every block the renderer supports, in one turn.
const RICH_MARKDOWN: &str = r#"# Element gallery

A turn that exercises **bold**, *italic*, ~~strikethrough~~, and `inline code`.

## Lists

- bullet one
- bullet two
  - nested bullet
1. ordered first
2. ordered second

> A blockquote line,
> spanning two rows.

| tool | kind  |
| ---- | ----- |
| bash | shell |
| read | file  |

```rust
fn demo(n: u32) -> u32 {
    n * 2
}
```

---

A [link](https://example.com) closes it out."#;

/// A realistic edit-tool result: preamble, then a `===DIFF===` section with
/// ` `/`+`/`-` prefixes (rendered as a colored diff).
const DIFF_RESULT: &str = "Previewing: replace\n\
    Note: the editor applies a consistent formatting style to the entire file\n\n\
    💡 TIP: For focused changes you might try targeted replace operations\n\n\
    ===DIFF===\n\n\
    \x20    /// True when the buffer holds no non-whitespace text.\n\
    \x20    pub(super) fn is_empty(&self) -> bool {\n\
    -        self.lines.iter().all(|l| l.iter().all(|c| c.is_whitespace()))\n\
    +        self.lines\n\
    +            .iter()\n\
    +            .all(|l| l.iter().all(|c| c.is_whitespace()))\n\
    \x20    }";

/// The fixed gallery transcript — one of each transcript element, in order.
pub(super) fn log() -> Vec<LogEntry> {
    let long_output = (1..=24)
        .map(|n| format!("src/module_{n:02}.rs"))
        .collect::<Vec<_>>()
        .join("\n");

    vec![
        // Rich markdown (user + assistant) and a bash call → shell block + result.
        user(1, "Show me everything the transcript can render."),
        assistant(2, RICH_MARKDOWN, vec![call("bash", "toolu_g1", serde_json::json!({
            "command": "grep -rn 'fn project' src/ | sort"
        }))]),
        result(3, "toolu_g1", "src/project.rs:12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {", false, WireState::Raw),

        // An edit tool's colored diff.
        assistant(4, "Here's a structured edit, shown as a colored diff:", vec![call(
            "preview_edit", "toolu_g2", serde_json::json!({ "anchor": "fn is_empty" }),
        )]),
        result(5, "toolu_g2", DIFF_RESULT, false, WireState::Raw),

        // A long result, collapsed under the disclosure triangle.
        assistant(6, "A long listing collapses behind a triangle:", vec![call(
            "bash", "toolu_g3", serde_json::json!({ "command": "ls src" }),
        )]),
        result(7, "toolu_g3", &long_output, false, WireState::Raw),

        // An error result (struck-through header).
        assistant(8, "And a failing command:", vec![call(
            "bash", "toolu_g4", serde_json::json!({ "command": "cargo test --no-run" }),
        )]),
        result(9, "toolu_g4", "error[E0599]: no method named `reproject` found\nerror: could not compile `efference`", true, WireState::Raw),

        // A distilled result (curated summary supersedes the raw output).
        assistant(10, "A distilled result shows its summary, not the raw bytes:", vec![call(
            "bash", "toolu_g5", serde_json::json!({ "command": "cargo tree" }),
        )]),
        result(11, "toolu_g5", "…600 lines of dependency tree…", false, WireState::Distilled(
            "12 direct deps; heaviest subtrees are tokio and trillium. Nothing outdated.".into(),
        )),

        // The file tools: concise call lines (path / pattern, not JSON) and a
        // `read` result with its sentinel framing stripped + highlighted by type.
        assistant(12, "The file tools render their arguments concisely:", vec![
            call("read", "toolu_g6", serde_json::json!({ "paths": ["src/tui/state.rs"] })),
            call("search", "toolu_g7", serde_json::json!({ "pattern": "fn project", "path": "src" })),
            call("list", "toolu_g8", serde_json::json!({ "path": "src/tui" })),
        ]),
        result(13, "toolu_g6", "==MK7 BEGIN /Users/jbr/code/efference/src/tui/state.rs, LENGTH: 96 MK7==\n\
            pub struct InteractiveState<'a> {\n    pub document: DocumentState<'a>,\n    pub viewport: ViewportState,\n}\n\
            ==MK7 END /Users/jbr/code/efference/src/tui/state.rs MK7==", false, WireState::Raw),
        result(14, "toolu_g7", "Found 1 match for pattern \"fn project\":\n\nsrc/project.rs:12: pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {", false, WireState::Raw),
        result(15, "toolu_g8", "All paths relative to src/tui:\n\n\ncomposer.rs\ndocument.rs\nprojection.rs", false, WireState::Raw),

        // A consolidation mark (horizontal rule + summary).
        entry(16, Payload::Mark(Mark {
            kind: MarkKind::Boundary { upto: 15 },
            note: "explored the renderer and confirmed the diff path".into(),
        })),
    ]
}
