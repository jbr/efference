//! The element gallery — a fixed "one of each" transcript for eyeballing the real
//! TUI (`efference dev gallery`), the terminal cousin of a component design page.
//!
//! It builds a [`LogEntry`] sequence that exercises every transcript shape — rich
//! markdown prose (headings, lists, quote, table, code, rule, inline styles), a
//! `bash` call rendered as a shell block, an edit tool's colored diff, a long
//! result collapsed under the disclosure triangle, an error result, a distilled
//! result, and a consolidation mark — then runs the real [`ui_loop`](super::ui_loop)
//! against it with no daemon behind it. The snapshot harness (`harness.rs`) covers
//! the same shapes headlessly; this is for seeing them live, in color, scrollable.

pub mod stream;

use crate::log::{
    AssistantOutput, ContentBlock, Keep, LogEntry, Mark, MarkKind, MessagesOutputBlock, Payload,
    RawResult, Timestamp, ToolCall, WireState,
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
            output: AssistantOutput::Messages(
                std::iter::once(MessagesOutputBlock::Text(text.into()))
                    .chain(calls.into_iter().map(MessagesOutputBlock::ToolUse))
                    .collect(),
            ),
            upstream_failure: None,
        },
    )
}

/// The same, with extended thinking ahead of the prose. Thinking renders as
/// markdown in the dimmed register, and the register's one real decision — code
/// keeping its hue where prose goes flat — is only visible in pixels, so the
/// gallery is where it can be looked at (docs/tui.md).
fn assistant_thinking(seq: u64, thinking: &str, text: &str) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::Messages(vec![
                MessagesOutputBlock::Thinking {
                    text: thinking.into(),
                    signature: String::new(),
                },
                MessagesOutputBlock::Text(text.into()),
            ]),
            upstream_failure: None,
        },
    )
}

/// A thinking block exercising the constructs the register has to handle: prose,
/// a list, inline code, emphasis, and a fenced block with real syntax.
const THINKING: &str = r#"Two ways to do this, and the difference is **byte stability**:

- reproject each turn — `project` is a pure fold, so it is cheap
- cache the prefix, which means tracking invalidation across `WireState` flips

The entry point is one function:

```rust
pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {
    let mut messages = Vec::new();
    for entry in log {
        // sorted keys keep the prefix byte-identical
        messages.extend(entry.wire());
    }
    messages
}
```

Reprojecting wins."#;

/// A committed tool result for `call_id`, at the given wire state.
fn result(seq: u64, call_id: &str, content: &str, is_error: bool, wire: WireState) -> LogEntry {
    let raw = RawResult::new(content, is_error);
    entry(seq, Payload::tool_result(call_id, raw, wire))
}

/// A committed tool result carrying one inline image — the offline shape, the
/// bytes riding the entry the way `persist::load` delivers them (docs/tui.md).
fn image_result(seq: u64, call_id: &str, content: &str) -> LogEntry {
    entry(
        seq,
        Payload::tool_result(
            call_id,
            RawResult::text(content).with_images(vec![test_card()]),
            WireState::Raw,
        ),
    )
}

/// A generated 600×400 test card: a horizontal hue sweep fading toward black at
/// the bottom, with a white ring. The gradients show color depth, the ring
/// shows aspect handling — a circle is where a wrong font-size guess becomes an
/// ellipse.
fn test_card() -> crate::log::ResultImage {
    use base64::Engine;
    let (w, h) = (600u32, 400u32);
    let hue = |t: f32, dim: f32| {
        let f = |n: f32| {
            let k = (n + t * 6.0) % 6.0;
            (1.0 - k.min(4.0 - k).clamp(0.0, 1.0)) * dim
        };
        [
            (f(5.0) * 255.0) as u8,
            (f(3.0) * 255.0) as u8,
            (f(1.0) * 255.0) as u8,
            255,
        ]
    };
    let img = ::image::RgbaImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 - 300.0, y as f32 - 200.0);
        let dist = (dx * dx + dy * dy).sqrt();
        if (dist - 150.0).abs() < 5.0 {
            ::image::Rgba([255, 255, 255, 255])
        } else {
            let dim = 1.0 - 0.7 * (y as f32 / h as f32);
            ::image::Rgba(hue(x as f32 / w as f32, dim))
        }
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgba8(img)
        .write_to(&mut bytes, ::image::ImageFormat::Png)
        .expect("png-encoding an in-memory buffer");
    crate::log::ResultImage {
        media_type: "image/png".into(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        width: w,
        height: h,
    }
}

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
        payload,
    }
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
        assistant(
            2,
            RICH_MARKDOWN,
            vec![ToolCall::new(
                "bash",
                "toolu_g1",
                serde_json::json!({
                    "command": "grep -rn 'fn project' src/ | sort"
                }),
            )],
        ),
        result(
            3,
            "toolu_g1",
            "src/project.rs:12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {",
            false,
            WireState::Raw,
        ),
        // An edit tool's colored diff.
        assistant(
            4,
            "Here's a structured edit, shown as a colored diff:",
            vec![ToolCall::new(
                "edit",
                "toolu_g2",
                serde_json::json!({
                    "file_path": "src/tui/composer.rs",
                    "operation": "replace",
                    "anchor": "fn is_empty",
                }),
            )],
        ),
        result(5, "toolu_g2", DIFF_RESULT, false, WireState::Raw),
        // A long result, collapsed under the disclosure triangle.
        assistant(
            6,
            "A long listing collapses behind a triangle:",
            vec![ToolCall::new(
                "bash",
                "toolu_g3",
                serde_json::json!({ "command": "ls src" }),
            )],
        ),
        result(7, "toolu_g3", &long_output, false, WireState::Raw),
        // An error result (struck-through header).
        assistant(
            8,
            "And a failing command:",
            vec![ToolCall::new(
                "bash",
                "toolu_g4",
                serde_json::json!({ "command": "cargo test --no-run" }),
            )],
        ),
        result(
            9,
            "toolu_g4",
            "error[E0599]: no method named `reproject` found\nerror: could not compile `efference`",
            true,
            WireState::Raw,
        ),
        // A distilled result (curated summary supersedes the raw output).
        assistant(
            10,
            "A distilled result shows its summary, not the raw bytes:",
            vec![ToolCall::new(
                "bash",
                "toolu_g5",
                serde_json::json!({ "command": "cargo tree" }),
            )],
        ),
        result(
            11,
            "toolu_g5",
            "…600 lines of dependency tree…",
            false,
            WireState::distilled(Keep::None).with_note(
                "12 direct deps; heaviest subtrees are tokio and trillium. Nothing outdated.",
            ),
        ),
        // The file tools: concise call lines (path / pattern, not JSON) and a
        // `read` result with its sentinel framing stripped + highlighted by type.
        assistant(
            12,
            "The file tools render their arguments concisely:",
            vec![
                ToolCall::new(
                    "read",
                    "toolu_g6",
                    serde_json::json!({ "paths": ["src/tui/state.rs"] }),
                ),
                ToolCall::new(
                    "search",
                    "toolu_g7",
                    serde_json::json!({ "pattern": "fn project", "path": "src" }),
                ),
                ToolCall::new("list", "toolu_g8", serde_json::json!({ "path": "src/tui" })),
            ],
        ),
        result(
            13,
            "toolu_g6",
            "==MK7 BEGIN /Users/jbr/code/efference/src/tui/state.rs, LENGTH: 96 MK7==\n\
            pub struct InteractiveState<'a> {\n    pub document: DocumentState<'a>,\n    pub viewport: ViewportState,\n}\n\
            ==MK7 END /Users/jbr/code/efference/src/tui/state.rs MK7==",
            false,
            WireState::Raw,
        ),
        result(
            14,
            "toolu_g7",
            "Found 1 match for pattern \"fn project\":\n\nsrc/project.rs:12: pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {",
            false,
            WireState::Raw,
        ),
        result(
            15,
            "toolu_g8",
            "All paths relative to src/tui:\n\n\ncomposer.rs\ndocument.rs\nprojection.rs",
            false,
            WireState::Raw,
        ),
        // A consolidation mark (horizontal rule + summary).
        entry(
            16,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 15 },
                note: "explored the renderer and confirmed the diff path".into(),
            }),
        ),
        // An image result (docs/tui.md): the generated test card rides the entry
        // inline, so the gallery exercises the full decode → encode → paint path
        // under whatever protocol this terminal answered — halfblocks included.
        assistant(
            17,
            "An image result renders through the terminal's graphics protocol:",
            vec![ToolCall::new(
                "read_image",
                "toolu_g9",
                serde_json::json!({ "path": "test-card.png" }),
            )],
        ),
        image_result(
            18,
            "toolu_g9",
            "600×400 image/png, 900 visual tokens (test card)",
        ),
        // Content below the image, so scrolling can carry it off the *top* —
        // the halfblocks row-slice path is unreachable when the image is the
        // last element (a bottom-pinned viewport only ever clips its bottom).
        user(
            19,
            "Scroll the card off the top — the reserved rectangle must hold.",
        ),
        assistant(
            20,
            "Partial visibility switches to the halfblocks slice, top or bottom: the \
             pixel protocols paint only from the image's top edge, and the reservation \
             never reflows — scrolling changes what paints, not what fits.\n\n\
             - fully visible → the terminal's own protocol, crisp\n\
             - partially visible → halfblock cells, blitted row by row\n\
             - `--images off` → a one-line reference, no reservation",
            vec![ToolCall::new(
                "bash",
                "toolu_g10",
                serde_json::json!({ "command": "seq 1 18 | sed 's/^/scroll ballast line /'" }),
            )],
        ),
        // Tall enough that the card scrolls off the top at ordinary widths —
        // prose alone only wraps tall on a narrow terminal.
        result(
            21,
            "toolu_g10",
            &(1..=18)
                .map(|n| format!("scroll ballast line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
            false,
            WireState::Raw,
        ),
        // Thinking: markdown in the dimmed register, code holding its hue.
        assistant_thinking(22, THINKING, "Reprojecting, then."),
    ]
}
