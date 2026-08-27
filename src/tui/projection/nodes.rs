//! Small document-node and span builders shared by the projection.

use super::partial::PartialInput;
use super::*;

/// A just-submitted user turn not yet echoed back as a committed `User` log entry
/// (docs/tui.md): the user's own voice (the accent gutter), but
/// **dimmed** and led by a `⋯` marker so the human sees the message landed and is in flight
/// — "handed to the daemon, not yet on the wire." It resolves to the normal committed
/// rendering the instant the matching `User` entry arrives via `/events`. Rendered as plain
/// dimmed lines (not markdown) so the in-flight echo doesn't reflow as it commits.
pub(in crate::tui) fn pending_turn(text: &str) -> DocumentNode<'static> {
    let mut lines: Vec<Span<'static>> = text
        .lines()
        .enumerate()
        // The marker leads the first line; continuation lines indent to align.
        .map(|(i, line)| dim_span(&format!("{}{line}", if i == 0 { "⋯ " } else { "  " })))
        .collect();
    // A blank submit can't reach here, but always emit a turn rather than an empty one.
    if lines.is_empty() {
        lines.push(dim_span("⋯ "));
    }
    DocumentNode::entry(
        EntryKind::UserMessage,
        vec![DocumentNode::styled_lines(lines)],
    )
}

/// The live phase, rendered **in full** — the point is to watch the work happen, so nothing here
/// truncates. Thinking renders as dimmed prose; a tool call renders as its arguments, with the one
/// value still arriving shown as content.
///
/// The input is a partial slice of a JSON document, so the display must not assert structure the
/// arrived bytes don't support (docs/tui.md). It is parsed as a *prefix*, and what may be claimed
/// from that is decided positionally — see [`calling_tool_blocks`] and `projection/partial.rs`.
/// Before any pair has arrived there is nothing to claim and the raw characters are shown, which
/// claim nothing.
pub(in crate::tui) fn phase_blocks(phase: &Phase) -> Vec<DocumentNode<'static>> {
    let Phase::CallingTool { name, input } = phase;
    if let Some(blocks) = calling_tool_blocks(name, input) {
        return blocks;
    }
    // Nothing structural has arrived yet (`{"pa`). Dimmed lines, not a `code_block`:
    // raw characters claim nothing, which is what makes showing them safe, and a box
    // would claim this is a finished thing to read rather than bytes still arriving.
    let mut lines = vec![dim_span(&format!(
        "❯ calling {}…",
        crate::mcp::display_name(name)
    ))];
    lines.extend(input.lines().map(|line| dim_span(&format!("  {line}"))));
    vec![DocumentNode::styled_lines(lines)]
}

/// A tool call rendered from the part of its input that has arrived (docs/tui.md):
/// the settled arguments as arguments, and the one value still in flight as
/// content. `None` before anything structural has arrived, so the caller falls back
/// to the raw characters.
///
/// The settled half goes through [`call_args`] — the same function a *committed*
/// call renders through — so the header fills in as bytes land rather than changing
/// shape when the call commits. The in-flight value never reaches it: what
/// [`call_args`] produces is an identity claim (`write src/api.rs`), and only a
/// value whose closing quote has arrived may back one
/// (`projection/partial.rs`).
fn calling_tool_blocks(name: &str, input: &str) -> Option<Vec<DocumentNode<'static>>> {
    let parsed = PartialInput::parse(input)?;
    if parsed.settled.is_empty() && parsed.arriving.is_none() {
        return None;
    }

    let mut header = vec![
        dim_span("❯ calling "),
        inline_code_styled(crate::mcp::display_name(name), SpanStyle::FunctionName),
    ];
    let settled = parsed.settled_value();
    let (inline, mut blocks) = call_args(name, &settled);
    if let Some(arg_spans) = inline {
        header.push(dim_span(" · "));
        header.extend(arg_spans);
    }
    header.push(dim_span("…"));

    // The call's `intent` (docs/tools.md) leads the body, as it does on a committed
    // call: the operator reads why before what. It settles early — it is emitted
    // first — so it is readable for the whole time the arguments are still arriving,
    // which is exactly the window this render exists for.
    if let Some(intent) = settled
        .get(crate::tools::INTENT)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|intent| !intent.is_empty())
    {
        blocks.insert(0, DocumentNode::styled_lines([dim_span(intent)]));
    }
    blocks.insert(0, DocumentNode::lines(vec![header]));

    // The value still arriving, as content. A string with newlines in it is the
    // case worth rendering as a block — a file's contents, a heredoc, a message
    // body — and it is rendered in the language of whatever settled path names it,
    // which is why the path arriving first is what makes this worth doing.
    if let Some((key, text)) = parsed.arriving_text() {
        if text.contains('\n') {
            blocks.push(code_block(streaming_lang(&parsed), text));
        } else {
            blocks.push(DocumentNode::styled_lines([dim_span(&format!(
                "  {key}: {text}…"
            ))]));
        }
    }
    Some(blocks)
}

/// The language to highlight an in-flight value in: taken from whichever settled
/// argument names a file. Only the settled ones are consulted — a path that is
/// itself still arriving names nothing yet.
fn streaming_lang(parsed: &PartialInput) -> Option<&'static str> {
    parsed
        .settled
        .iter()
        .find(|(key, _)| matches!(key.as_str(), "path" | "file_path"))
        .and_then(|(_, value)| value.as_str())
        .and_then(lang_for_path)
}

/// One dimmed span — the live tail's register throughout: present but visibly not
/// committed. A *span*, not a node, because the live tail's text is line-structured
/// rather than block-structured; see [`DocumentNode::Lines`].
pub(in crate::tui) fn dim_span(text: &str) -> Span<'static> {
    Span {
        text: text.to_string().into(),
        style: SpanStyle::Comment,
        action: None,
    }
}

/// One dimmed line as a *block* — separated from its siblings by a blank row. For
/// text that really is paragraph-structured; line-structured text wants
/// [`DocumentNode::Lines`] instead.
pub(in crate::tui) fn dim_line(text: &str) -> DocumentNode<'static> {
    DocumentNode::Paragraph {
        spans: vec![dim_span(text)],
    }
}

/// A turn's extended thinking, as an entry that stays in the transcript
/// (docs/tui.md). `None` when the turn did no thinking.
///
/// **Shown in full.** Thinking streams past far too fast to read at generation
/// speed, so the live phase is only a "something is happening" signal and this is
/// the copy a human actually reads, at their own pace, afterwards — which it can
/// only be if all of it is here. Whether some of it is reliably worth hiding is a
/// question to answer from reading real sessions, not to assume up front
/// (docs/positions.md).
///
/// Rendered in the dimmed register, like the live phase it succeeds: thinking is not
/// the assistant's answer and must never be mistaken for it.
pub(in crate::tui) fn thinking_entry(thinking: &[ContentBlock]) -> Option<DocumentNode<'static>> {
    if thinking.is_empty() {
        return None;
    }
    let mut body: Vec<DocumentNode<'static>> = Vec::new();
    for block in thinking {
        match block {
            ContentBlock::Thinking { text, .. } => {
                if let Some(lines) = thinking_lines(text) {
                    body.push(lines);
                }
            }
            // Safety-encrypted thinking is opaque — say so rather than rendering nothing, so the
            // record doesn't silently omit that the model thought here.
            ContentBlock::RedactedThinking { .. } => {
                body.push(dim_line("(encrypted by the provider — not readable)"));
            }
            ContentBlock::Text(_) => {}
        }
    }
    (!body.is_empty()).then(|| thinking_block(body))
}

/// The same entry, built from thinking **still streaming** (docs/tui.md). Identical
/// in shape to the committed one on purpose: the live copy is superseded by the
/// committed copy when the turn lands, and a block that changed shape at that moment
/// would read as the transcript rewriting itself.
pub(in crate::tui) fn live_thinking_entry(text: &str) -> Option<DocumentNode<'static>> {
    Some(thinking_block(vec![thinking_lines(text)?]))
}

/// A thinking text as markdown, in the dimmed register. `None` for text that is
/// empty or only whitespace.
///
/// Thinking goes through the same [`MarkdownRenderer`] assistant prose does. It
/// is not *required* to be markdown, any more than prose is — it simply is:
/// across 653 blocks in 20 sessions, 73.5% carry an explicit markdown construct.
/// Rendering it as literal text left fences, inline code, emphasis, and lists as
/// the characters that spell them, which the web client (already doing this)
/// made obvious by comparison.
///
/// This *fixes* the regression the previous implementation was defending
/// against. Keeping the model's own line structure was a response to reflowing
/// `- a\n- b` into `- a - b`; markdown rendering resolves that properly by
/// making it a list, rather than by refusing to reflow anything.
///
/// The dimming is a wrapper, not a per-span choice, because the register is a
/// property of where the node sits — see [`DocumentNode::Dimmed`].
fn thinking_lines(text: &str) -> Option<DocumentNode<'static>> {
    if text.trim().is_empty() {
        return None;
    }
    let nodes = markdown(text.trim_matches('\n'));
    (!nodes.is_empty()).then(|| DocumentNode::dimmed(nodes))
}

/// The `✻ thinking` entry both the live and committed renders produce.
fn thinking_block(body: Vec<DocumentNode<'static>>) -> DocumentNode<'static> {
    DocumentNode::headed_entry(
        EntryKind::Thinking,
        None,
        vec![dim_span("✻ thinking")],
        body,
    )
}

/// The affordance shown beside a pending call (docs/tui.md), where its result will
/// land once the gate resolves. The accent-colored lead-in marks "your turn"; the muted
/// tail names the composer's two direct answers — the full verb set (allow…/abort) lives
/// on the control strip, which is the legend now.
///
/// It carries the `⏸` marker, on an entry of its own: the call's marker says what the
/// agent did, and a gate is something the *harness* did about it.
pub(in crate::tui) fn gate_affordance() -> DocumentNode<'static> {
    DocumentNode::entry(
        EntryKind::Annotation,
        vec![DocumentNode::Paragraph {
            spans: vec![
                Span {
                    text: "⏸ awaiting your approval".into(),
                    style: SpanStyle::UserAccent,
                    action: None,
                },
                Span {
                    text: "  ·  ⏎ approve · type to redirect".into(),
                    style: SpanStyle::Comment,
                    action: None,
                },
            ],
        }],
    )
}

/// One harness annotation beside a call or its result: a marker glyph and a muted
/// line, as its own non-focusable [`Annotation`](EntryKind::Annotation) entry
/// (docs/tui.md). The shared shape behind the spotter's verdict and the distiller's
/// note — both are a different rung's account *of* the content, not the content.
pub(in crate::tui) fn annotation_line(marker: &str, text: &str) -> DocumentNode<'static> {
    DocumentNode::entry(
        EntryKind::Annotation,
        vec![DocumentNode::Paragraph {
            spans: vec![
                Span {
                    text: marker.to_string().into(),
                    style: SpanStyle::Comment,
                    action: None,
                },
                Span {
                    text: text.to_string().into(),
                    style: SpanStyle::Comment,
                    action: None,
                },
            ],
        }],
    )
}

/// The spotter's verdict on a call (docs/permissions.md), as one muted line. Before the
/// decision it is *why* the call is held; after, it is a gloss on what the call was taken
/// to be — so it stays in the dim register either way and never competes with the call or
/// its result for attention.
///
/// `stance` is `None` only for a gate whose `Spot` entry hasn't reached this client yet
/// (the live `/state` path, which carries the reason but not the stance); the committed
/// entry supplies it in every other case. It leads the line because it is the part that is
/// scannable — a `cleared` gloss and a `gated` one are read very differently, and without
/// it every verdict looks alike. [`Origin`](crate::log::Origin) is deliberately not drawn:
/// a fail-closed verdict's own reason says "spotter unavailable", so surfacing it again
/// would cost a glyph to repeat what the line already reads.
///
/// Emitted as its own [`Annotation`](EntryKind::Annotation) entry beside the call it
/// judges, never inside it: it is a separate rung's judgment *about* the call rather
/// than part of the call's content. It is not a focus stop — it is read beside the
/// call, not visited.
pub(in crate::tui) fn spotter_line(
    stance: Option<crate::log::Stance>,
    reason: &str,
) -> DocumentNode<'static> {
    use crate::log::Stance;
    let label = match stance {
        // `ClearAlways` refines `Clear` with a prefix-promotion vote (docs/permissions.md).
        // That vote is policy machinery, not a fact about this call, so both read `cleared`.
        Some(Stance::Clear | Stance::ClearAlways) => "spotter cleared · ",
        Some(Stance::Watch) => "spotter watching · ",
        Some(Stance::Gate) => "spotter gated · ",
        None => "spotter · ",
    };
    DocumentNode::entry(
        EntryKind::Annotation,
        vec![DocumentNode::Paragraph {
            spans: vec![
                Span {
                    text: label.into(),
                    style: SpanStyle::Comment,
                    action: None,
                },
                Span {
                    text: reason.to_string().into(),
                    style: SpanStyle::Comment,
                    action: None,
                },
            ],
        }],
    )
}

/// The call marker: `❯`, always. A call is a call regardless of how it went — what
/// happened to it is the *result*'s marker ([`status_span`]).
pub(in crate::tui) fn prompt_span() -> Span<'static> {
    Span {
        text: "❯ ".into(),
        style: SpanStyle::Comment,
        action: None,
    }
}

/// The result marker: `✓` at rest, `✗` (red) when the call failed — so an error
/// reads as an error without an `[error]` word, on the entry that actually failed.
pub(in crate::tui) fn status_span(is_error: bool) -> Span<'static> {
    if is_error {
        Span {
            text: "✗ ".into(),
            style: SpanStyle::Error,
            action: None,
        }
    } else {
        Span {
            text: "✓ ".into(),
            style: SpanStyle::Comment,
            action: None,
        }
    }
}

/// A running tool's output so far — a **peek**, not a view of the output: the last
/// [`PEEK_LINES`] lines, dimmed, so the operator can see the process is alive and
/// roughly where it is. Unlike a committed result (which renders whole), live output
/// is unbounded and untriaged, and the entry is replaced by the real result the
/// moment the call lands.
pub(in crate::tui) fn running_tool_peek(output: &str) -> DocumentNode<'static> {
    let lines: Vec<&str> = output.lines().collect();
    let shown = lines.len().min(PEEK_LINES);
    let header = vec![Span {
        text: match lines.len() {
            n if n > shown => format!("running · {n} lines, last {shown}").into(),
            1 => "running · 1 line".into(),
            n => format!("running · {n} lines").into(),
        },
        style: SpanStyle::Comment,
        action: None,
    }];
    let body = vec![DocumentNode::styled_lines(
        lines[lines.len() - shown..]
            .iter()
            .map(|line| dim_span(&format!("  {line}"))),
    )];
    DocumentNode::headed_entry(
        EntryKind::ToolResult,
        Some(Span {
            text: "⋯ ".into(),
            style: SpanStyle::Comment,
            action: None,
        }),
        header,
        body,
    )
}

/// How many trailing lines of a *running* tool's output the peek shows. Small on
/// purpose: this is a glance at a live process, not a way to read its output. The
/// whole output arrives, untruncated, when the call commits.
const PEEK_LINES: usize = 10;

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
