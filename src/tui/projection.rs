//! Project the append-only log (plus the in-flight streaming tail) into a
//! renderable [`Document`] (docs/README.md — the transcript is another `project(log)`,
//! a sibling of the wire projection in `crate::project`).
//!
//! This mirrors two rules from the wire projection ([`crate::project`]): a tool
//! result shows its *distillate* when present, else its raw content
//! ([`tool_block`]); and long output is collapsible (here via a
//! [`DocumentNode::TruncatedBlock`] rather than the wire's `[seq]` addressing).
//! Unlike the wire projection it does not coalesce or elide — every turn renders,
//! so the human sees the whole record.

use std::collections::{HashMap, HashSet};

use crate::log::{ContentBlock, LogEntry, MarkKind, Payload, RawResult, ToolCall, WireState};
use crate::tui::document::{
    Author, DiffLine, DiffLineKind, Document, DocumentNode, ListItem, Span, SpanStyle,
    TruncationLevel,
};
use crate::tui::markdown::MarkdownRenderer;

/// The in-flight streaming deltas not yet committed to the log: the assistant's
/// prose so far this turn, and each running tool's accumulated output (keyed by
/// `call_id`, kept in arrival order).
#[derive(Debug, Default)]
pub(crate) struct LiveTail {
    pub assistant: String,
    tools: Vec<(String, String)>,
}

impl LiveTail {
    /// Append a fragment of a running tool's output.
    pub(crate) fn push_tool_output(&mut self, call_id: &str, chunk: &str) {
        if let Some((_, buf)) = self.tools.iter_mut().find(|(id, _)| id == call_id) {
            buf.push_str(chunk);
        } else {
            self.tools.push((call_id.to_string(), chunk.to_string()));
        }
    }

    /// A committed entry supersedes the live fragments it was streaming: clear the
    /// assistant buffer when an assistant turn lands, and drop a tool's buffer when
    /// its result lands. (Mirrors `main.rs`'s `streamed_*` dedup flags, now
    /// structural.)
    pub(crate) fn commit(&mut self, entry: &LogEntry) {
        match &entry.payload {
            Payload::Assistant { .. } => self.assistant.clear(),
            Payload::ToolResult { call_id, .. } => {
                self.tools.retain(|(id, _)| id != &call_id.0);
            }
            _ => {}
        }
    }
}

/// Per-exchange view-state the human has toggled, carried in `tui::state` so it
/// survives re-projection (docs/tui.md). The transcript rebuilds on every
/// streamed token, so a block's collapse/raw state can't live in the rebuilt
/// [`Document`] — it lives here, keyed by the result's `call_id`, and `project`
/// applies it when building each exchange.
#[derive(Debug, Default)]
pub(crate) struct ViewState {
    /// `call_id`s whose exchange the human expanded (`▼` Full instead of `▶` Brief).
    expanded: HashSet<String>,
    /// `call_id`s the human flipped to **raw** output — the wire distillate is the
    /// default, raw is shown on demand (both are already client-side; this is a
    /// pure render choice, no round-trip).
    raw: HashSet<String>,
}

impl ViewState {
    fn is_expanded(&self, call_id: &str) -> bool {
        self.expanded.contains(call_id)
    }

    fn is_raw(&self, call_id: &str) -> bool {
        self.raw.contains(call_id)
    }

    /// Flip the expand state for a `call_id`, returning the new state. The
    /// [`Document`]'s level is recomputed from this on the next projection.
    pub(crate) fn toggle_expanded(&mut self, call_id: &str) -> bool {
        toggle(&mut self.expanded, call_id)
    }

    /// Flip raw-vs-wire for a `call_id`, returning whether it now shows raw.
    pub(crate) fn toggle_raw(&mut self, call_id: &str) -> bool {
        toggle(&mut self.raw, call_id)
    }
}

/// Flip a `call_id`'s membership in a set, returning whether it is now present.
fn toggle(set: &mut HashSet<String>, call_id: &str) -> bool {
    if set.remove(call_id) {
        false
    } else {
        set.insert(call_id.to_string());
        true
    }
}

/// Fold the log + live tail into a transcript document of conversational
/// [`Turn`](DocumentNode::Turn)s. Authorship is structural (the renderer draws a
/// gutter for the user; the assistant is unmarked), and each tool call is paired
/// with its result into one collapsible *exchange* (`❯ tool · arg` over the
/// result) rather than two separately-labeled blocks. `view` carries the human's
/// per-exchange toggles (expand, raw-vs-wire) across this rebuild (docs/tui.md).
///
/// `pending_call` is the `call_id` the agent is *blocked on for approval* (docs/tui.md).
/// The loop appends the assistant `tool_use` **before** it gates (so the
/// call is already in the log), and one tool call runs per turn (docs/README.md) — so the
/// pending call is always the tail result-less exchange. That exchange renders
/// *as if invoked*, with a `⏸` marker and an inline affordance where the result
/// will land, so the gate reads as "after the call, before the result" rather than
/// a banner divorced from the call it's gating.
///
/// `pending_turns` are just-submitted user messages not yet echoed back as committed
/// `User` log entries (docs/tui.md). They render **after** the
/// committed log and live tail, dimmed with a `⋯` marker — the optimistic "handed to the
/// daemon, not yet on the wire" echo — and are dropped as their matching `User` entry
/// arrives via `/events`.
pub(crate) fn project(
    log: &[LogEntry],
    live: &LiveTail,
    view: &ViewState,
    pending_call: Option<&str>,
    pending_reason: Option<&str>,
    pending_turns: &[String],
) -> Document<'static> {
    // A result lands in a later entry than the call it answers; index results by
    // `call_id` up front so a call can be paired with its result in one place.
    let mut result_for_call: HashMap<&str, (&RawResult, &WireState)> = HashMap::new();
    for entry in log {
        if let Payload::ToolResult { call_id, raw, wire } = &entry.payload {
            result_for_call.insert(call_id.0.as_str(), (raw, wire));
        }
    }

    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();
    for entry in log {
        match &entry.payload {
            Payload::User(blocks) => {
                nodes.push(turn(Author::User, markdown(&join_text(blocks))));
            }
            Payload::Assistant { text, calls, .. } => {
                let mut blocks = Vec::new();
                let prose = join_text(text);
                if !prose.trim().is_empty() {
                    blocks.extend(markdown(&prose));
                }
                for call in calls {
                    let result = result_for_call.get(call.id.0.as_str()).copied();
                    let is_pending = result.is_none() && pending_call == Some(call.id.0.as_str());
                    let reason = if is_pending { pending_reason } else { None };
                    blocks.push(exchange(call, result, view, is_pending, reason));
                }
                if !blocks.is_empty() {
                    nodes.push(turn(Author::Assistant, blocks));
                }
            }
            // Folded into its call's exchange above.
            Payload::ToolResult { .. } => {}
            Payload::Mark(mark) => push_mark(&mut nodes, &mark.kind, &mark.note),
            // An ambient notification (docs/inbox.md), rendered as a marked line so the
            // human sees the out-of-band event in the stream. The transcript shows the
            // whole record — every *shown* notification renders here, where the wire
            // carries only the pinned ones.
            Payload::Ambient(n) => nodes.push(DocumentNode::Paragraph {
                spans: vec![
                    Span {
                        text: format!("⟢ {} · ", n.source).into(),
                        style: SpanStyle::Comment,
                        action: None,
                    },
                    Span::plain(n.body.clone()),
                ],
            }),
            // The verdict is internal — audit-only on the transcript (docs/redaction-fork.md).
            Payload::Triage(_) => {}
            // A spotter permission verdict (docs/permissions.md) — audit-only, like Triage.
            Payload::Spot(_) => {}
            // A grant (backlog/permissions/config-edit.md) — audit-only on the transcript
            // for now; surfacing scope changes to the human lands with the construction sites.
            Payload::ConfigEdit(_) => {}
            // The boot preamble snapshot (docs/context.md) — session boot record,
            // not conversation; the human inspects it via `efference wire`, not the transcript.
            Payload::Preamble { .. } => {}
        }
    }

    // The live tail renders after everything committed: in-flight assistant prose
    // as its own turn, and each running tool as a result-less exchange.
    if !live.assistant.is_empty() {
        nodes.push(turn(Author::Assistant, markdown(&live.assistant)));
    }
    for (call_id, buf) in &live.tools {
        let header = vec![inline_code(call_id.clone())];
        nodes.push(turn(
            Author::Assistant,
            vec![DocumentNode::collapsible(
                header,
                Some(prompt_span(false)),
                vec![code_block(None, buf)],
                TruncationLevel::Brief,
            )],
        ));
    }

    // Optimistic echoes of just-submitted turns, last of all (docs/tui.md).
    for text in pending_turns {
        nodes.push(pending_turn(text));
    }

    Document::with_nodes(nodes)
}

/// A just-submitted user turn not yet echoed back as a committed `User` log entry
/// (docs/tui.md): the user's own voice (the accent gutter), but
/// **dimmed** and led by a `⋯` marker so the human sees the message landed and is in flight
/// — "handed to the daemon, not yet on the wire." It resolves to the normal committed
/// rendering the instant the matching `User` entry arrives via `/events`. Rendered as plain
/// dimmed lines (not markdown) so the in-flight echo doesn't reflow as it commits.
fn pending_turn(text: &str) -> DocumentNode<'static> {
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

/// Wrap a turn's blocks under its author.
fn turn(author: Author, blocks: Vec<DocumentNode<'static>>) -> DocumentNode<'static> {
    DocumentNode::Turn { author, blocks }
}

/// One tool call paired with its result as a collapsible exchange: the header is
/// the call (`❯ tool · arg`, or `✗ tool …` when the result errored); the body is
/// the (tool-specifically rendered) result. No `result #N` label — the result is
/// obviously the response to the call directly above it.
fn exchange(
    call: &ToolCall,
    result: Option<(&RawResult, &WireState)>,
    view: &ViewState,
    is_pending: bool,
    pending_reason: Option<&str>,
) -> DocumentNode<'static> {
    let is_error = result.is_some_and(|(raw, _)| raw.is_error);
    let call_id = call.id.0.as_str();

    let mut header = vec![inline_code_styled(
        call.name.clone(),
        SpanStyle::FunctionName,
    )];
    let (inline, overflow) = call_args(call);
    if let Some(arg_spans) = inline {
        header.push(Span {
            text: " · ".into(),
            style: SpanStyle::Comment,
            action: None,
        });
        header.extend(arg_spans);
    }

    let mut body = overflow;
    if let Some((raw, wire)) = result {
        // The human can flip a distilled result to its full raw output (docs/tui.md)
        // — both are already client-side, so this is a pure render choice. A
        // forced-raw view passes a synthetic `Raw` wire so `result_body` gives it the
        // same rich tool-specific rendering (diff/read) a natural raw result gets.
        let show_raw = view.is_raw(call_id);
        let overriding = show_raw && !matches!(wire, WireState::Raw);
        let (content, effective_wire) = if show_raw {
            (raw.content.clone(), &WireState::Raw)
        } else {
            let content = match wire {
                WireState::Raw => raw.content.clone(),
                // Same renderer as the wire projection — re-derive the distillate from
                // `raw` + the stored verdict (docs/redaction-fork.md).
                WireState::Distilled { keep, note } => {
                    crate::project::render_distilled(&raw.content, keep, note.as_deref())
                }
                WireState::Elided => "[elided]".to_string(),
            };
            (content, wire)
        };
        // Flag that the leaner distilled default is being overridden, so the human
        // sees they're on raw (`r` to flip back).
        if overriding {
            header.push(Span {
                text: " · raw".into(),
                style: SpanStyle::Comment,
                action: None,
            });
        }
        body.extend(result_body(
            Some(call.name.as_str()),
            raw,
            effective_wire,
            &content,
        ));
    } else if is_pending {
        // No result yet — the agent is blocked here for approval (docs/tui.md). The
        // affordance sits where the result will land, so the gate reads as "after the
        // call, before the result." Resolving it (approve → result; redirect/abort →
        // an error result) replaces this with the real exchange body. The spotter's
        // reason (docs/permissions.md), if any, renders just above it — *why* it gated.
        if let Some(reason) = pending_reason {
            body.push(spotter_reason_line(reason));
        }
        body.push(gate_affordance());
    }

    // The resting marker: `⏸` (accent) when this call is awaiting approval, else the
    // `❯`/`✗` prompt (the renderer swaps in a ▶/▼ disclosure triangle when the body
    // is long enough to collapse). The block is keyed by `call_id` so its expand/raw
    // state survives re-projection.
    let resting = if is_pending {
        Span {
            text: "⏸ ".into(),
            style: SpanStyle::UserAccent,
            action: None,
        }
    } else {
        prompt_span(is_error)
    };
    let level = if view.is_expanded(call_id) {
        TruncationLevel::Full
    } else {
        TruncationLevel::Brief
    };
    DocumentNode::collapsible_keyed(call_id.to_string(), header, Some(resting), body, level)
}

/// The inline affordance shown in a pending exchange's body, in place of the
/// not-yet-existent result (docs/tui.md). The accent-colored lead-in marks "your
/// turn"; the muted tail spells out how to answer through the composer.
fn gate_affordance() -> DocumentNode<'static> {
    DocumentNode::Paragraph {
        spans: vec![
            Span {
                text: "awaiting your approval".into(),
                style: SpanStyle::UserAccent,
                action: None,
            },
            Span {
                text: "  ·  ⏎ approve · type to redirect · .allow to add a rule · .abort".into(),
                style: SpanStyle::Comment,
                action: None,
            },
        ],
    }
}

/// The spotter's reason for holding a call, rendered just above the gate affordance
/// (docs/permissions.md) so the human sees *why* before deciding. Muted — context, not
/// a command.
fn spotter_reason_line(reason: &str) -> DocumentNode<'static> {
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
fn prompt_span(is_error: bool) -> Span<'static> {
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
fn markdown(text: &str) -> Vec<DocumentNode<'static>> {
    MarkdownRenderer::render(text)
}

/// A plain (or language-tagged) code block from owned content.
fn code_block(lang: Option<&str>, code: &str) -> DocumentNode<'static> {
    DocumentNode::CodeBlock {
        lang: lang.map(|l| l.to_string().into()),
        code: code.to_string().into(),
    }
}

/// The call's argument, split into what goes inline on the `❯ tool · …` header
/// (a short summary) and what overflows into the exchange body (a multi-line
/// `bash` command, or a JSON envelope for an unrecognized tool). Known tools get a
/// tool-specific summary — the file tools surface their path/pattern; `bash` shows
/// a single-line command inline, a multi-line one as a shell block.
fn call_args(call: &ToolCall) -> (Option<Vec<Span<'static>>>, Vec<DocumentNode<'static>>) {
    let input = |key| call.input.get(key).and_then(|v| v.as_str());
    match call.name.as_str() {
        n if n == crate::bash::NAME => {
            if let Some(command) = input("command") {
                return if command.contains('\n') {
                    (None, vec![code_block(Some("bash"), command)])
                } else {
                    (Some(vec![inline_code(command)]), vec![])
                };
            }
        }
        "read" => {
            if let Some(paths) = call.input.get("paths").and_then(|v| v.as_array()) {
                let joined = paths
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return (Some(vec![inline_code(joined)]), vec![]);
            }
        }
        "search" => {
            if let Some(pattern) = input("pattern") {
                let mut spans = vec![inline_code(pattern.to_string())];
                if let Some(path) = input("path") {
                    spans.push(Span::plain("  in  "));
                    spans.push(inline_code(path.to_string()));
                }
                return (Some(spans), vec![]);
            }
        }
        "list" => {
            if let Some(path) = input("path") {
                return (Some(vec![inline_code(path.to_string())]), vec![]);
            }
        }
        name if is_diff_tool(name) => {
            // The diff result is the representation — never dump the (verbose) edit
            // input. Show a concise anchor hint inline if present, else just the name.
            let inline = input("anchor").map(|anchor| {
                let snippet: String = anchor
                    .lines()
                    .next()
                    .unwrap_or(anchor)
                    .chars()
                    .take(48)
                    .collect();
                vec![inline_code(snippet)]
            });
            return (inline, vec![]);
        }
        _ => {}
    }
    let pretty =
        serde_json::to_string_pretty(&call.input).unwrap_or_else(|_| call.input.to_string());
    (None, vec![code_block(Some("json"), &pretty)])
}

/// An inline-code-styled span (paths, patterns, commands).
fn inline_code(text: impl Into<String>) -> Span<'static> {
    inline_code_styled(text, SpanStyle::InlineCode)
}

/// A span with an explicit style (header tool name, inline args).
fn inline_code_styled(text: impl Into<String>, style: SpanStyle) -> Span<'static> {
    Span {
        text: text.into().into(),
        style,
        action: None,
    }
}

/// The body shown under a tool-result header. Known tools get a tool-specific
/// representation of their raw output — an edit tool's `===DIFF===` becomes a
/// colored [`DocumentNode::Diff`], and a `read` result drops its sentinel framing
/// and highlights by file type. Distilled / elided / error results, and anything
/// unrecognized, fall back to the content as a plain code block.
fn result_body(
    tool: Option<&str>,
    raw: &RawResult,
    wire: &WireState,
    content: &str,
) -> Vec<DocumentNode<'static>> {
    if !raw.is_error && matches!(wire, WireState::Raw) {
        match tool {
            Some(t) if is_diff_tool(t) => {
                if let Some(diff) = parse_diff(content) {
                    return vec![diff];
                }
            }
            Some("read") => {
                if let Some(body) = read_result(content) {
                    return vec![body];
                }
            }
            _ => {}
        }
    }
    vec![code_block(None, content)]
}

/// The known tools whose result carries a `===DIFF===` section.
fn is_diff_tool(name: &str) -> bool {
    matches!(name, "edit" | "undo_edit" | "retarget_edit")
}

/// Strip a `read` result's sentinel framing (`==<token> BEGIN <path>, LENGTH: n
/// <token>==` … matching `END`) and render the file body as a code block, syntax-
/// highlighted by the path's extension. Returns `None` if the marker isn't present
/// (so the caller falls back to a plain code block).
fn read_result(content: &str) -> Option<DocumentNode<'static>> {
    let mut path: Option<String> = None;
    let mut saw_marker = false;
    let mut body: Vec<&str> = Vec::new();
    for line in content.lines() {
        match read_marker(line) {
            Some(found_path) => {
                saw_marker = true;
                if found_path.is_some() {
                    path = found_path;
                }
            }
            None => body.push(line),
        }
    }
    if !saw_marker {
        return None;
    }
    let lang = path.as_deref().and_then(lang_for_path);
    Some(code_block(lang, &body.join("\n")))
}

/// Classify a `read` sentinel line: `Some(Some(path))` for a `BEGIN` line,
/// `Some(None)` for an `END` line, `None` for ordinary file content.
fn read_marker(line: &str) -> Option<Option<String>> {
    let inner = line.strip_prefix("==")?.strip_suffix("==")?;
    if let Some(rest) = inner.split_once(" BEGIN ").map(|(_, r)| r) {
        let path = rest.split(", LENGTH:").next().unwrap_or(rest).trim();
        Some(Some(path.to_string()))
    } else if inner.contains(" END ") {
        Some(None)
    } else {
        None
    }
}

/// A syntect language token for a file path's extension, or `None` for plain text.
fn lang_for_path(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('.').next()? {
        "rs" => "rust",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "sh" | "bash" => "bash",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" => "css",
        _ => return None,
    })
}

/// Parse an edit tool's result into a colored diff: take the text after the
/// `===DIFF===` marker and classify each line by its leading ` `/`+`/`-`/`@`.
/// Returns `None` when there's no diff section (so the caller falls back to a
/// plain code block).
fn parse_diff(content: &str) -> Option<DocumentNode<'static>> {
    let body = content.split_once("===DIFF===")?.1.trim_start_matches('\n');
    if body.trim().is_empty() {
        return None;
    }
    let lines = body
        .lines()
        .map(|line| {
            let kind = match line.chars().next() {
                Some('+') => DiffLineKind::Added,
                Some('-') => DiffLineKind::Removed,
                Some('@') => DiffLineKind::Hunk,
                _ => DiffLineKind::Context,
            };
            DiffLine {
                kind,
                text: line.to_string(),
            }
        })
        .collect();
    Some(DocumentNode::diff(lines))
}

fn push_mark(nodes: &mut Vec<DocumentNode<'static>>, kind: &MarkKind, note: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{CallId, Keep, RawResult, Timestamp, ToolCall};

    fn entry(seq: u64, payload: Payload) -> LogEntry {
        LogEntry {
            seq,
            turn: 1,
            at: Timestamp(0),
            payload,
        }
    }

    /// A user turn, an assistant turn with markdown + a tool call, and a tool
    /// result — the three shapes the transcript is built from.
    fn sample_log() -> Vec<LogEntry> {
        vec![
            entry(
                1,
                Payload::User(vec![ContentBlock::Text("hello there".into())]),
            ),
            entry(
                2,
                Payload::Assistant {
                    thinking: vec![],
                    text: vec![ContentBlock::Text("**bold** plan".into())],
                    calls: vec![ToolCall {
                        id: CallId("toolu_1".into()),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "echo hi" }),
                    }],
                },
            ),
            entry(
                3,
                Payload::ToolResult {
                    call_id: CallId("toolu_1".into()),
                    raw: RawResult {
                        content: "line one\nline two".into(),
                        is_error: false,
                    },
                    wire: WireState::Raw,
                },
            ),
        ]
    }

    #[test]
    fn projects_turns_and_pairs_call_with_result() {
        let doc = project(
            &sample_log(),
            &LiveTail::default(),
            &ViewState::default(),
            None,
            None,
            &[],
        );

        // Top level is a sequence of authored turns: user, then assistant.
        let authors: Vec<Author> = doc
            .nodes
            .iter()
            .filter_map(|n| match n {
                DocumentNode::Turn { author, .. } => Some(*author),
                _ => None,
            })
            .collect();
        assert_eq!(authors, vec![Author::User, Author::Assistant]);

        // The assistant turn holds prose plus exactly one exchange (the call paired
        // with its result) — not two separately-labeled blocks.
        let assistant_blocks = doc
            .nodes
            .iter()
            .find_map(|n| match n {
                DocumentNode::Turn {
                    author: Author::Assistant,
                    blocks,
                } => Some(blocks),
                _ => None,
            })
            .expect("assistant turn");
        let exchanges = assistant_blocks
            .iter()
            .filter(|n| matches!(n, DocumentNode::TruncatedBlock { .. }))
            .count();
        assert_eq!(
            exchanges, 1,
            "one exchange, not separate call + result blocks"
        );

        // The exchange header names the tool, with no role/seq label.
        let header_text: String = assistant_blocks
            .iter()
            .filter_map(|n| match n {
                DocumentNode::TruncatedBlock { header, .. } => header.as_ref(),
                _ => None,
            })
            .flatten()
            .map(|s| s.text.as_ref())
            .collect();
        assert!(
            header_text.contains("bash"),
            "tool name in header: {header_text}"
        );
        assert!(
            !header_text.contains("result #"),
            "no result label: {header_text}"
        );
        assert!(!header_text.contains("wants to run"), "call already ran");

        // The result content lives in the exchange body.
        let code = collect_code(&doc.nodes);
        assert!(code.contains("line one"), "result body present: {code}");
    }

    #[test]
    fn distilled_wire_supersedes_raw() {
        // A call and its distilled result (a result is only rendered as part of its
        // call's exchange, so the call must be present).
        let log = vec![
            entry(
                4,
                Payload::Assistant {
                    thinking: vec![],
                    text: vec![],
                    calls: vec![ToolCall {
                        id: CallId("toolu_9".into()),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "noise" }),
                    }],
                },
            ),
            entry(
                5,
                Payload::ToolResult {
                    call_id: CallId("toolu_9".into()),
                    raw: RawResult {
                        content: "RAW-NOISE\nthe kept line".into(),
                        is_error: false,
                    },
                    wire: WireState::Distilled {
                        keep: Keep::Ranges(vec![(2, 2)]),
                        note: Some("the distilled summary".into()),
                    },
                },
            ),
        ];
        let doc = project(
            &log,
            &LiveTail::default(),
            &ViewState::default(),
            None,
            None,
            &[],
        );
        let code: String = collect_code(&doc.nodes);
        assert!(
            code.contains("the distilled summary"),
            "shows the note: {code}"
        );
        assert!(
            code.contains("the kept line"),
            "shows the kept line: {code}"
        );
        assert!(
            !code.contains("RAW-NOISE"),
            "hides the dropped raw line: {code}"
        );
    }

    /// The (single) exchange block's expand level, for the view-state tests.
    fn exchange_level(doc: &Document<'_>) -> TruncationLevel {
        for node in &doc.nodes {
            if let DocumentNode::Turn { blocks, .. } = node {
                for b in blocks {
                    if let DocumentNode::TruncatedBlock { level, .. } = b {
                        return *level;
                    }
                }
            }
        }
        panic!("no exchange block in the document");
    }

    /// The (single) exchange block's header text, for the view-state tests.
    fn exchange_header(doc: &Document<'_>) -> String {
        for node in &doc.nodes {
            if let DocumentNode::Turn { blocks, .. } = node {
                for b in blocks {
                    if let DocumentNode::TruncatedBlock {
                        header: Some(h), ..
                    } = b
                    {
                        return h.iter().map(|s| s.text.as_ref()).collect();
                    }
                }
            }
        }
        String::new()
    }

    #[test]
    fn expand_state_lives_in_the_view_and_survives_reprojection() {
        // The docs/tui.md fix: the expand level is read from the view, not stored in the
        // rebuilt document — so it survives the re-projection a streamed token forces
        // (where the old in-document level reset to Brief).
        let log = sample_log(); // one bash exchange, call id toolu_1
        let mut view = ViewState::default();

        // Default: collapsed.
        let doc = project(&log, &LiveTail::default(), &view, None, None, &[]);
        assert_eq!(exchange_level(&doc), TruncationLevel::Brief);

        // Expanding persists in the view.
        view.toggle_expanded("toolu_1");
        let doc = project(&log, &LiveTail::default(), &view, None, None, &[]);
        assert_eq!(exchange_level(&doc), TruncationLevel::Full);

        // A second projection (the streamed-token rebuild) keeps it expanded.
        let doc = project(&log, &LiveTail::default(), &view, None, None, &[]);
        assert_eq!(
            exchange_level(&doc),
            TruncationLevel::Full,
            "expand survived reprojection"
        );

        // Toggling back collapses, and that persists too.
        view.toggle_expanded("toolu_1");
        let doc = project(&log, &LiveTail::default(), &view, None, None, &[]);
        assert_eq!(exchange_level(&doc), TruncationLevel::Brief);
    }

    #[test]
    fn raw_toggle_shows_full_raw_over_the_distillate() {
        // A distilled result (the same shape as `distilled_wire_supersedes_raw`).
        let log = vec![
            entry(
                4,
                Payload::Assistant {
                    thinking: vec![],
                    text: vec![],
                    calls: vec![ToolCall {
                        id: CallId("toolu_9".into()),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "noise" }),
                    }],
                },
            ),
            entry(
                5,
                Payload::ToolResult {
                    call_id: CallId("toolu_9".into()),
                    raw: RawResult {
                        content: "RAW-NOISE\nthe kept line".into(),
                        is_error: false,
                    },
                    wire: WireState::Distilled {
                        keep: Keep::Ranges(vec![(2, 2)]),
                        note: Some("the distilled summary".into()),
                    },
                },
            ),
        ];
        let mut view = ViewState::default();

        // Default: the distillate, raw hidden, no `· raw` marker on the header.
        let doc = project(&log, &LiveTail::default(), &view, None, None, &[]);
        let code = collect_code(&doc.nodes);
        assert!(code.contains("the kept line"), "distillate shown: {code}");
        assert!(
            !code.contains("RAW-NOISE"),
            "dropped raw line hidden: {code}"
        );
        assert!(
            !exchange_header(&doc).contains("raw"),
            "no raw marker by default"
        );

        // Flip to raw: the full raw content, and the header flags the override.
        view.toggle_raw("toolu_9");
        let doc = project(&log, &LiveTail::default(), &view, None, None, &[]);
        let code = collect_code(&doc.nodes);
        assert!(code.contains("RAW-NOISE"), "full raw now shown: {code}");
        assert!(
            exchange_header(&doc).contains("raw"),
            "header marks the raw view"
        );
    }

    #[test]
    fn live_tail_renders_after_committed_entries_and_clears_on_commit() {
        let mut live = LiveTail::default();
        live.assistant.push_str("streaming…");
        live.push_tool_output("toolu_1", "partial out");

        let doc = project(&sample_log(), &live, &ViewState::default(), None, None, &[]);
        let code = collect_code(&doc.nodes);
        assert!(code.contains("partial out"), "live tool output renders");

        // Committing the assistant entry clears the live assistant buffer.
        live.commit(&entry(
            4,
            Payload::Assistant {
                thinking: vec![],
                text: vec![ContentBlock::Text("done".into())],
                calls: vec![],
            },
        ));
        assert!(
            live.assistant.is_empty(),
            "assistant buffer cleared on commit"
        );
    }

    /// Render the projection to a ratatui TestBackend and confirm the transcript
    /// text reaches the screen (mirrors ferritin's `render_to_test_backend`).
    #[test]
    fn renders_transcript_to_a_test_backend() {
        use crate::tui::render_context::{OutputMode, RenderContext};
        use crate::tui::state::InteractiveState;
        use crate::tui::theme::InteractiveTheme;
        use ratatui::{Terminal, backend::TestBackend};

        let rc = RenderContext::new()
            .with_interactive(true)
            .with_output_mode(OutputMode::Tty)
            .with_terminal_width(80);
        let theme = InteractiveTheme::from_render_context(&rc);
        let (_tx, rx) = crossbeam_channel::unbounded();
        let (submit_tx, _submit_rx) = tokio::sync::mpsc::unbounded_channel();
        let (approve_tx, _approve_rx) = tokio::sync::mpsc::unbounded_channel();
        let (pace_tx, _pace_rx) = tokio::sync::mpsc::unbounded_channel();
        let (interrupt_tx, _interrupt_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state =
            InteractiveState::new(rx, submit_tx, approve_tx, pace_tx, interrupt_tx, rc, theme);
        state.document.document = project(
            &sample_log(),
            &LiveTail::default(),
            &ViewState::default(),
            None,
            None,
            &[],
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 60)).unwrap();
        terminal.draw(|f| state.render_frame(f)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                screen.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(screen.contains("hello there"), "user prompt on screen");
        assert!(screen.contains("plan"), "assistant prose on screen");
        // The exchange header names the tool; its result body sits in the (short,
        // shown) exchange below it.
        assert!(screen.contains("bash"), "exchange header on screen");
    }

    fn collect_code(nodes: &[DocumentNode<'_>]) -> String {
        let mut out = String::new();
        for node in nodes {
            match node {
                DocumentNode::CodeBlock { code, .. } => {
                    out.push_str(code);
                    out.push('\n');
                }
                DocumentNode::TruncatedBlock { nodes, .. } => out.push_str(&collect_code(nodes)),
                DocumentNode::Turn { blocks, .. } => out.push_str(&collect_code(blocks)),
                _ => {}
            }
        }
        out
    }
}
