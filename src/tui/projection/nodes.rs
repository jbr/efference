//! Small document-node and span builders shared by the projection.

use super::partial::PartialInput;
use super::*;

/// A just-submitted user turn not yet echoed back as a committed `User` log entry
/// (docs/tui.md): the user's own voice (the accent gutter), but
/// **dimmed** and led by a `⋯` marker so the human sees the message landed and is in flight
/// — "handed to the daemon, not yet on the wire." It resolves to the normal committed
/// rendering the instant the matching `User` entry arrives via `/events`. Rendered as plain
/// dimmed lines (not markdown) so the in-flight echo doesn't reflow as it commits.
pub(in crate::tui) fn pending_turn(
    text: &str,
    attachments: &[crate::attachments::Arrival],
) -> DocumentNode<'static> {
    let mut lines: Vec<Span<'static>> = text
        .lines()
        .enumerate()
        // The marker leads the first line; continuation lines indent to align.
        .map(|(i, line)| dim_span(&format!("{}{line}", if i == 0 { "⋯ " } else { "  " })))
        .collect();
    // An attachments-only turn has no text line: its first attachment carries the marker.
    if lines.is_empty() {
        if !attachments.is_empty() {
            let mut body = attachment_spans(attachments);
            body[0] = dim_span(&format!("⋯ {}", body[0].text));
            return DocumentNode::entry(
                EntryKind::UserMessage,
                vec![DocumentNode::styled_lines(body)],
            );
        }
        lines.push(dim_span("⋯ "));
    }
    let mut body = vec![DocumentNode::styled_lines(lines)];
    body.extend(attachment_lines(attachments));
    DocumentNode::entry(EntryKind::UserMessage, body)
}

/// The files a user turn carried, one dimmed line each (docs/tui.md): name and what the daemon
/// made of it. `None` when the turn carried none.
pub(in crate::tui) fn attachment_lines(
    attachments: &[crate::attachments::Arrival],
) -> Option<DocumentNode<'static>> {
    (!attachments.is_empty()).then(|| DocumentNode::styled_lines(attachment_spans(attachments)))
}

fn attachment_spans(attachments: &[crate::attachments::Arrival]) -> Vec<Span<'static>> {
    attachments
        .iter()
        .map(|a| {
            dim_span(&format!(
                "⎘ {} · {}",
                a.name,
                crate::tui::render_staged::kind_detail(&a.kind)
            ))
        })
        .collect()
}

/// One in-flight tool call, rendered in full (docs/tui.md): its settled arguments as
/// arguments, the one value still arriving as content.
///
/// Keyed by the `call_id` the call's result will key on, which binds the live block to
/// its call before commit — what any per-entry view state needs to survive the next
/// streamed fragment. What may be claimed from a prefix is decided positionally; see
/// [`calling_tool_blocks`] and `projection/partial.rs`.
pub(in crate::tui) fn live_call_entry(call: &LiveCall) -> DocumentNode<'static> {
    let entry = calling_tool_entry(&call.name, &call.input).unwrap_or_else(|| {
        // Nothing structural has arrived yet (`{"pa`). Dimmed lines, not a `code_block`:
        // raw characters claim nothing, which is what makes showing them safe, and a box
        // would claim this is a finished thing to read rather than bytes still arriving.
        let mut lines = vec![dim_span(&format!(
            "› {}…",
            crate::mcp::display_name(&call.name)
        ))];
        lines.extend(
            call.input
                .lines()
                .map(|line| dim_span(&format!("  {line}"))),
        );
        DocumentNode::entry(
            EntryKind::AssistantMessage,
            vec![DocumentNode::styled_lines(lines)],
        )
    });
    entry.keyed(call.call_id.clone())
}

/// A tool call rendered from the part of its input that has arrived (docs/tui.md).
/// `None` before anything structural has arrived, so the caller falls back to the raw
/// characters.
///
/// The settled half goes through [`call_blocks`], the same function a *committed*
/// call renders through, so the call fills in as bytes land rather than changing
/// shape at commit: the intent is the title, then the signature, then the bodies.
/// The in-flight value never reaches the signature: it produces an identity claim
/// (`write("src/api.rs")`), and only a value whose closing quote has arrived may back
/// one (`projection/partial.rs`) — except the intent, which claims nothing about what
/// the call touches and fills in as the title while it streams.
fn calling_tool_entry(name: &str, input: &str) -> Option<DocumentNode<'static>> {
    let parsed = PartialInput::parse(input)?;
    if parsed.settled.is_empty() && parsed.arriving.is_none() {
        return None;
    }
    let settled = parsed.settled_value();
    let view = super::signature::call_view(name, &settled);
    let arriving = parsed.arriving_text();
    let intent = view.intent.clone().or_else(|| {
        arriving
            .filter(|(key, _)| *key == crate::tools::INTENT)
            .map(|(_, text)| text.to_string())
    });
    let mut blocks = call_blocks(view, true);

    // The value still arriving, as content. A string with newlines in it is the
    // case worth rendering as a block — a file's contents, a heredoc, a message
    // body — and it is rendered in the language of whatever settled path names it,
    // which is why the path arriving first is what makes this worth doing.
    if let Some((key, text)) = arriving.filter(|(key, _)| *key != crate::tools::INTENT) {
        if text.contains('\n') {
            blocks.push(code_block(streaming_lang(&parsed), text));
        } else {
            blocks.push(DocumentNode::styled_lines([dim_span(&format!(
                "  {key}: {text}…"
            ))]));
        }
    }
    Some(match intent {
        Some(intent) => DocumentNode::headed_entry(
            EntryKind::AssistantMessage,
            None,
            intent_header(&intent),
            blocks,
        ),
        None => DocumentNode::entry(EntryKind::AssistantMessage, blocks),
    })
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
/// (docs/tui.md). `None` when the turn did no thinking. Shown in full, in the dimmed
/// register, like the live tail it succeeds.
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
            ContentBlock::Text(_) | ContentBlock::Attachment(_) => {}
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

/// The child's held move, shown beside a bound `sessions__approve` so the human
/// approves the move rather than an id (docs/permissions.md). The logged call line
/// stays exactly as the model wrote it; this annotation names which child and what
/// it is holding. The chain joins with `→` when the approval passed through more
/// than one session.
pub(in crate::tui) fn bound_action(bound: &crate::tui::ActingOn) -> DocumentNode<'static> {
    let display = crate::mcp::display_name(&bound.name);
    let detail = match bound.name.as_str() {
        "bash" => bound
            .input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => serde_json::to_string(&bound.input).unwrap_or_default(),
    };
    let target = if bound.chain.len() > 1 {
        bound.chain.join(" → ")
    } else {
        bound.session.clone()
    };
    let mut spans = vec![
        Span {
            text: "⤷ acting on ".into(),
            style: SpanStyle::Comment,
            action: None,
        },
        Span {
            text: format!("{target}'s {display}").into(),
            style: SpanStyle::UserAccent,
            action: None,
        },
    ];
    if !detail.is_empty() {
        spans.push(Span {
            text: format!(" · {detail}").into(),
            style: SpanStyle::Comment,
            action: None,
        });
    }
    if let Some(intent) = bound.intent.as_deref().filter(|i| !i.trim().is_empty()) {
        spans.push(Span {
            text: format!(" — {intent}").into(),
            style: SpanStyle::Comment,
            action: None,
        });
    }
    DocumentNode::entry(
        EntryKind::Annotation,
        vec![DocumentNode::Paragraph { spans }],
    )
}

/// One harness annotation beside a call or its result: a marker glyph and a muted
/// line, as its own non-focusable [`Annotation`](EntryKind::Annotation) entry
/// (docs/tui.md). The shared shape behind the spotter's verdict and the triager's
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

/// The notice a `Worktree` entry carries (docs/worktrees.md), as one muted
/// [`Annotation`](EntryKind::Annotation) line in the shape a `Resumed` entry
/// uses, with the branch in the inline-code register the web client chips it in.
///
/// Git's setup `output` is deliberately not shown: the line states the outcome,
/// and a failed setup's exit status is on the line itself because that failure is
/// the reason the entry exists. The whole output is in the log (`efference dev`),
/// and surfacing it is backlog/inbox/worktree-setup-detail.md.
pub(in crate::tui) fn worktree_notice(event: &crate::log::WorktreeEvent) -> DocumentNode<'static> {
    use crate::log::WorktreeEventKind;
    let mut spans = vec![dim_span("⟢ worktree ")];
    match &event.kind {
        WorktreeEventKind::Added(checkout) => {
            spans.push(dim_span("added on "));
            spans.push(inline_code(event.branch.clone()));
            spans.push(dim_span(&setup_note(checkout.exit_code)));
        }
        WorktreeEventKind::Restored(checkout) => {
            spans.push(dim_span("restored from "));
            spans.push(inline_code(event.branch.clone()));
            spans.push(dim_span(&setup_note(checkout.exit_code)));
        }
        WorktreeEventKind::Removed { branch_deleted } => {
            spans.push(dim_span(if *branch_deleted {
                "removed (branch deleted)"
            } else {
                "removed (branch kept)"
            }));
        }
        WorktreeEventKind::Kept { reason } => {
            spans.push(dim_span(&format!("kept: {reason}")));
        }
        WorktreeEventKind::RemoveFailed { error } => {
            spans.push(dim_span(&format!("removal failed: {error}")));
        }
    }
    DocumentNode::entry(
        EntryKind::Annotation,
        vec![DocumentNode::Paragraph { spans }],
    )
}

/// How a checkout's exit status reads on the notice line: nothing when the setup
/// succeeded, the status otherwise. A nonzero status is stated on the line
/// itself, since a failed setup is the reason the entry exists (docs/worktrees.md).
/// Mirrors the web client's `setupNote` (`web/src/ui/worktree.ts`).
fn setup_note(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(0) => String::new(),
        Some(code) => format!(" (setup failed: exit {code})"),
        None => " (setup failed: git ended by signal)".to_string(),
    }
}

/// The spotter's verdict on a call (docs/permissions.md, docs/tui.md), as one muted
/// line, emitted as its own [`Annotation`](EntryKind::Annotation) entry beside the
/// call it judges.
///
/// `stance` is `None` only for a gate whose `Spot` entry hasn't reached this client yet
/// (the `gate` state topic carries the reason but not the stance); the committed entry
/// supplies it in every other case.
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

/// A running tool's output so far — by default a **peek**, not a view of the output:
/// the last [`PEEK_LINES`] lines, dimmed, so the operator can see the process is
/// alive and roughly where it is. Unlike a committed result (which renders whole),
/// live output is unbounded and untriaged, and the entry is replaced by the real
/// result the moment the call lands.
///
/// `expanded` is the operator's per-call choice to read the whole output instead
/// (the block menu's `full` verb). It is held in [`ViewState`] by `call_id`, which
/// is also this entry's durable `key` — the entry is rebuilt on every arriving
/// fragment, so a flag stored anywhere in the document would be discarded by the
/// next one.
pub(in crate::tui) fn running_tool_peek(
    call_id: &str,
    output: &str,
    expanded: bool,
) -> DocumentNode<'static> {
    let lines: Vec<&str> = output.lines().collect();
    let shown = if expanded {
        lines.len()
    } else {
        lines.len().min(PEEK_LINES)
    };
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
        EntryKind::RunningTool,
        Some(Span {
            text: "⋯ ".into(),
            style: SpanStyle::Comment,
            action: None,
        }),
        header,
        body,
    )
    .keyed(call_id.to_string())
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

/// A summary (docs/summary.md), drawn as the web client draws it
/// (docs/web-client.md): a rule naming its extent — `⊙ consolidated through #n ·
/// N entries` — then the summary that replaced the span, rendered as the markdown
/// it is, then a closing rule. Every entry it replaced is still on screen above,
/// so there is nothing to expand: the rule says where the model's context stops
/// carrying the raw log. `replaces` counts log entries, not tokens, which are
/// provider accounting the client does not hold.
pub(in crate::tui) fn push_summary(
    nodes: &mut Vec<DocumentNode<'static>>,
    summary: &Summary,
    replaces: usize,
) {
    let label = summary.label();
    nodes.push(DocumentNode::horizontal_rule());
    nodes.push(DocumentNode::Paragraph {
        spans: vec![
            Span {
                text: "⊙ ".into(),
                style: SpanStyle::UserAccent,
                action: None,
            },
            Span {
                text: format!(
                    "{label} · {replaces} {}",
                    if replaces == 1 { "entry" } else { "entries" }
                )
                .into(),
                style: SpanStyle::Comment,
                action: None,
            },
        ],
    });
    let body = markdown(&summary.text);
    if !body.is_empty() {
        nodes.push(DocumentNode::entry(EntryKind::AssistantMessage, body));
    }
    nodes.push(DocumentNode::horizontal_rule());
}

/// A turn's text as markdown, led by the `source:` of a turn another client
/// submitted (docs/control-api.md). The prefix is metadata, so it is a span of its
/// own ahead of the first paragraph rather than text the markdown parser sees —
/// where a source like `mcp[claude-code]` read as a link reference and lost its
/// brackets. A body that opens with something other than a paragraph gets the
/// source on a line of its own.
pub(in crate::tui) fn sourced_markdown(
    source: Option<&str>,
    text: &str,
) -> Vec<DocumentNode<'static>> {
    let mut nodes = if text.is_empty() {
        Vec::new()
    } else {
        markdown(text)
    };
    let Some(source) = source else {
        return nodes;
    };
    let prefix = Span {
        text: format!("{source}: ").into(),
        style: SpanStyle::Comment,
        action: None,
    };
    match nodes.first_mut() {
        Some(DocumentNode::Paragraph { spans }) => spans.insert(0, prefix),
        _ => nodes.insert(0, DocumentNode::paragraph(vec![prefix])),
    }
    nodes
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
            ContentBlock::Thinking { .. }
            | ContentBlock::RedactedThinking { .. }
            | ContentBlock::Attachment(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
