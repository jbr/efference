//! Small document-node and span builders shared by the projection.

use super::*;

/// A just-submitted user turn not yet echoed back as a committed `User` log entry
/// (docs/tui.md): the user's own voice (the accent gutter), but
/// **dimmed** and led by a `⋯` marker so the human sees the message landed and is in flight
/// — "handed to the daemon, not yet on the wire." It resolves to the normal committed
/// rendering the instant the matching `User` entry arrives via `/events`. Rendered as plain
/// dimmed lines (not markdown) so the in-flight echo doesn't reflow as it commits.
pub(in crate::tui) fn pending_turn(text: &str) -> DocumentNode<'static> {
    let mut blocks: Vec<DocumentNode<'static>> = text
        .lines()
        .enumerate()
        .map(|(i, line)| DocumentNode::Paragraph {
            spans: vec![Span {
                // The marker leads the first line; continuation lines indent to align.
                text: format!("{}{line}", if i == 0 { "⋯ " } else { "  " }).into(),
                style: SpanStyle::Comment,
                action: None,
            }],
        })
        .collect();
    // A blank submit can't reach here, but always emit a turn rather than an empty one.
    if blocks.is_empty() {
        blocks.push(DocumentNode::Paragraph {
            spans: vec![Span {
                text: "⋯ ".into(),
                style: SpanStyle::Comment,
                action: None,
            }],
        });
    }
    turn(Author::User, blocks)
}

/// The live phase, rendered **in full** — the point is to watch the work happen, so nothing here
/// truncates. Thinking renders as dimmed prose; a tool call renders as its name plus the JSON input
/// accumulating in a code block.
///
/// The input is shown, never *parsed*: mid-stream it is a partial slice of a JSON document, so the
/// display must not assert structure the arrived bytes don't support. Rendering it structurally is
/// its own design question ([`backlog/tui-streaming-lifecycle.md`]) — the raw characters claim
/// nothing, which is what makes showing them safe.
///
/// [`backlog/tui-streaming-lifecycle.md`]: ../../backlog/tui-streaming-lifecycle.md
pub(in crate::tui) fn phase_blocks(phase: &Phase) -> Vec<DocumentNode<'static>> {
    match phase {
        Phase::Thinking { text } if text.trim().is_empty() => vec![dim_line("✻ thinking…")],
        Phase::Thinking { text } => {
            // The marker leads the first line; continuation lines indent to align under it, the
            // same shape a pending user turn uses.
            let mut blocks = Vec::new();
            for (i, line) in text.lines().enumerate() {
                blocks.push(dim_line(&format!(
                    "{}{line}",
                    if i == 0 { "✻ thinking… " } else { "  " }
                )));
            }
            blocks
        }
        Phase::CallingTool { name, input } if input.is_empty() => {
            vec![dim_line(&format!("❯ calling {name}…"))]
        }
        Phase::CallingTool { name, input } => {
            // Dimmed lines, not a `code_block`: the boxed render doesn't wrap its content to the
            // box's inner width, so a long input would be *clipped* at the border — precisely the
            // content this exists to show (the open box-wrap defect in dogfooding-friction.md).
            let mut blocks = vec![dim_line(&format!("❯ calling {name}…"))];
            blocks.extend(input.lines().map(|line| dim_line(&format!("  {line}"))));
            blocks
        }
    }
}

/// One dimmed line — the live tail's register throughout: present but visibly not committed.
pub(in crate::tui) fn dim_line(text: &str) -> DocumentNode<'static> {
    DocumentNode::Paragraph {
        spans: vec![Span {
            text: text.to_string().into(),
            style: SpanStyle::Comment,
            action: None,
        }],
    }
}

/// Wrap a turn's blocks under its author.
pub(in crate::tui) fn turn(
    author: Author,
    blocks: Vec<DocumentNode<'static>>,
) -> DocumentNode<'static> {
    DocumentNode::Turn { author, blocks }
}

/// The prefix a thinking block's `key` carries. Two kinds of block are keyed —
/// a tool exchange by its `call_id`, a thinking block by `thinking:<seq>` — and
/// this is what tells them apart (see [`is_call_key`]).
pub(in crate::tui) const THINKING_KEY_PREFIX: &str = "thinking:";

/// A turn's extended thinking, as a collapsed block that stays in the transcript
/// (docs/tui.md). `None` when the turn did no thinking.
///
/// **Collapsed by default, but never hidden.** A collapsed block still shows its opening lines
/// (with a `(+N lines)` hint), so the gist of the reasoning is permanently visible and the whole of
/// it is one keypress away. That is the point: thinking streams past far too fast to read live, so
/// the live phase is only a "something is happening" signal — this is the part a human actually
/// reads, at their own pace, afterwards.
///
/// Rendered in the dimmed register, like the live phase it succeeds: thinking is not the
/// assistant's answer and must never be mistaken for it. The block is keyed by the entry's `seq`
/// (thinking has no `call_id`), which is what carries its expand state across the re-projection
/// that every streamed token triggers.
pub(in crate::tui) fn thinking_block(
    seq: crate::log::Seq,
    thinking: &[ContentBlock],
    view: &ViewState,
) -> Option<DocumentNode<'static>> {
    if thinking.is_empty() {
        return None;
    }
    let mut body: Vec<DocumentNode<'static>> = Vec::new();
    for block in thinking {
        match block {
            ContentBlock::Thinking { text, .. } => {
                // Split on blank lines, not every newline: a blank line is the model's own
                // paragraph break, and one node per source line would double-space the whole
                // block (the renderer separates body children by a blank row).
                body.extend(
                    text.split("\n\n")
                        .map(str::trim)
                        .filter(|para| !para.is_empty())
                        .map(|para| dim_line(&para.replace('\n', " "))),
                );
            }
            // Safety-encrypted thinking is opaque — say so rather than rendering nothing, so the
            // record doesn't silently omit that the model thought here.
            ContentBlock::RedactedThinking { .. } => {
                body.push(dim_line("(encrypted by the provider — not readable)"));
            }
            ContentBlock::Text(_) => {}
        }
    }
    if body.is_empty() {
        return None;
    }
    let key = format!("{THINKING_KEY_PREFIX}{seq}");
    let level = if view.is_expanded(&key) {
        TruncationLevel::Full
    } else {
        TruncationLevel::Brief
    };
    Some(DocumentNode::collapsible_keyed(
        key,
        vec![Span {
            text: "✻ thinking".into(),
            style: SpanStyle::Comment,
            action: None,
        }],
        None,
        body,
        level,
    ))
}

/// The inline affordance shown in a pending exchange's body, in place of the
/// not-yet-existent result (docs/tui.md). The accent-colored lead-in marks "your
/// turn"; the muted tail names the composer's two direct answers — the full verb
/// set (allow…/abort) lives on the control strip, which is the legend now.
pub(in crate::tui) fn gate_affordance() -> DocumentNode<'static> {
    DocumentNode::Paragraph {
        spans: vec![
            Span {
                text: "awaiting your approval".into(),
                style: SpanStyle::UserAccent,
                action: None,
            },
            Span {
                text: "  ·  ⏎ approve · type to redirect".into(),
                style: SpanStyle::Comment,
                action: None,
            },
        ],
    }
}

/// The spotter's reason for holding a call (docs/permissions.md), so the human sees *why*
/// before deciding. Muted — context, not a command.
///
/// Emitted as a **sibling** of the exchange it judges, never inside it: it is a separate
/// rung's judgment *about* the call rather than part of the call's content, and nesting it
/// meant the exchange's own truncation could clip it (which it did, for any command long
/// enough to fill the collapsed body).
pub(in crate::tui) fn spotter_reason_line(reason: &str) -> DocumentNode<'static> {
    DocumentNode::Paragraph {
        spans: vec![
            Span {
                text: "spotter: ".into(),
                style: SpanStyle::Comment,
                action: None,
            },
            Span {
                text: reason.to_string().into(),
                style: SpanStyle::Comment,
                action: None,
            },
        ],
    }
}

/// The exchange prompt glyph: `❯` for a normal call, `✗` (red) when its result
/// failed — so an error reads as an error without a `[error]` word.
pub(in crate::tui) fn prompt_span(is_error: bool) -> Span<'static> {
    if is_error {
        Span {
            text: "✗ ".into(),
            style: SpanStyle::Error,
            action: None,
        }
    } else {
        Span {
            text: "❯ ".into(),
            style: SpanStyle::Comment,
            action: None,
        }
    }
}

/// Markdown-render owned prose into nodes. `markdown::render` builds owned spans,
/// so the result is `'static`.
pub(in crate::tui) fn markdown(text: &str) -> Vec<DocumentNode<'static>> {
    MarkdownRenderer::render(text)
}

/// A plain (or language-tagged) code block from owned content.
pub(in crate::tui) fn code_block(lang: Option<&str>, code: &str) -> DocumentNode<'static> {
    DocumentNode::CodeBlock {
        lang: lang.map(|l| l.to_string().into()),
        code: code.to_string().into(),
    }
}

/// An inline-code-styled span (paths, patterns, commands).
pub(in crate::tui) fn inline_code(text: impl Into<String>) -> Span<'static> {
    inline_code_styled(text, SpanStyle::InlineCode)
}

/// A span with an explicit style (header tool name, inline args).
pub(in crate::tui) fn inline_code_styled(
    text: impl Into<String>,
    style: SpanStyle,
) -> Span<'static> {
    Span {
        text: text.into().into(),
        style,
        action: None,
    }
}

pub(in crate::tui) fn push_mark(
    nodes: &mut Vec<DocumentNode<'static>>,
    kind: &MarkKind,
    note: &str,
) {
    nodes.push(DocumentNode::horizontal_rule());
    let MarkKind::Boundary { upto } = kind;
    nodes.push(DocumentNode::List {
        start: None,
        items: vec![ListItem::new(vec![DocumentNode::Paragraph {
            spans: vec![
                Span {
                    text: format!("consolidated through #{upto}: ").into(),
                    style: SpanStyle::Comment,
                    action: None,
                },
                Span::plain(note.to_string()),
            ],
        }])],
    });
}

/// Join text blocks with blank lines so paragraph structure survives. Only `Text` blocks
/// contribute — thinking blocks render through their own (future) surface, not the prose join.
/// `pub(crate)` so the state layer can match a committed `User` entry against its optimistic
/// pending echo (docs/tui.md) using the same text extraction.
pub(crate) fn join_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.as_str()),
            ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
