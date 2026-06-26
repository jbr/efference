//! Project the append-only log (plus the in-flight streaming tail) into a
//! renderable [`Document`] (DESIGN §10 — the transcript is another `project(log)`,
//! a sibling of the wire projection in `crate::project`).
//!
//! This mirrors two rules from the wire projection ([`crate::project`]): a tool
//! result shows its *distillate* when present, else its raw content
//! ([`tool_block`]); and long output is collapsible (here via a
//! [`DocumentNode::TruncatedBlock`] rather than the wire's `[seq]` addressing).
//! Unlike the wire projection it does not coalesce or elide — every turn renders,
//! so the human sees the whole record.

use std::collections::HashMap;

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

/// Fold the log + live tail into a transcript document of conversational
/// [`Turn`](DocumentNode::Turn)s. Authorship is structural (the renderer draws a
/// gutter for the user; the assistant is unmarked), and each tool call is paired
/// with its result into one collapsible *exchange* (`❯ tool · arg` over the
/// result) rather than two separately-labeled blocks.
pub(crate) fn project(log: &[LogEntry], live: &LiveTail) -> Document<'static> {
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
            Payload::Assistant { text, calls } => {
                let mut blocks = Vec::new();
                let prose = join_text(text);
                if !prose.trim().is_empty() {
                    blocks.extend(markdown(&prose));
                }
                for call in calls {
                    let result = result_for_call.get(call.id.0.as_str()).copied();
                    blocks.push(exchange(call, result));
                }
                if !blocks.is_empty() {
                    nodes.push(turn(Author::Assistant, blocks));
                }
            }
            // Folded into its call's exchange above.
            Payload::ToolResult { .. } => {}
            Payload::Mark(mark) => push_mark(&mut nodes, &mark.kind, &mark.note),
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

    Document::with_nodes(nodes)
}

/// Wrap a turn's blocks under its author.
fn turn(author: Author, blocks: Vec<DocumentNode<'static>>) -> DocumentNode<'static> {
    DocumentNode::Turn { author, blocks }
}

/// One tool call paired with its result as a collapsible exchange: the header is
/// the call (`❯ tool · arg`, or `✗ tool …` when the result errored); the body is
/// the (tool-specifically rendered) result. No `result #N` label — the result is
/// obviously the response to the call directly above it.
fn exchange(call: &ToolCall, result: Option<(&RawResult, &WireState)>) -> DocumentNode<'static> {
    let is_error = result.is_some_and(|(raw, _)| raw.is_error);

    let mut header = vec![inline_code_styled(call.name.clone(), SpanStyle::FunctionName)];
    let (inline, overflow) = call_args(call);
    if let Some(arg_spans) = inline {
        header.push(Span { text: " · ".into(), style: SpanStyle::Comment, action: None });
        header.extend(arg_spans);
    }

    let mut body = overflow;
    if let Some((raw, wire)) = result {
        let content = match wire {
            WireState::Raw => raw.content.clone(),
            WireState::Distilled(s) => s.clone(),
            WireState::Elided => "[elided]".to_string(),
        };
        body.extend(result_body(Some(call.name.as_str()), raw, wire, &content));
    }

    // The `❯`/`✗` prompt is the block's resting marker (the renderer swaps in a
    // ▶/▼ disclosure triangle when the result is long enough to collapse).
    DocumentNode::collapsible(header, Some(prompt_span(is_error)), body, TruncationLevel::Brief)
}

/// The exchange prompt glyph: `❯` for a normal call, `✗` (red) when its result
/// failed — so an error reads as an error without a `[error]` word.
fn prompt_span(is_error: bool) -> Span<'static> {
    if is_error {
        Span { text: "✗ ".into(), style: SpanStyle::Error, action: None }
    } else {
        Span { text: "❯ ".into(), style: SpanStyle::Comment, action: None }
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
                let joined = paths.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ");
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
                let snippet: String = anchor.lines().next().unwrap_or(anchor).chars().take(48).collect();
                vec![inline_code(snippet)]
            });
            return (inline, vec![]);
        }
        _ => {}
    }
    let pretty = serde_json::to_string_pretty(&call.input).unwrap_or_else(|_| call.input.to_string());
    (None, vec![code_block(Some("json"), &pretty)])
}

/// An inline-code-styled span (paths, patterns, commands).
fn inline_code(text: impl Into<String>) -> Span<'static> {
    inline_code_styled(text, SpanStyle::InlineCode)
}

/// A span with an explicit style (header tool name, inline args).
fn inline_code_styled(text: impl Into<String>, style: SpanStyle) -> Span<'static> {
    Span { text: text.into().into(), style, action: None }
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
    matches!(name, "preview_edit" | "persist_edit" | "retarget_edit")
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
            DiffLine { kind, text: line.to_string() }
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

/// Join text blocks with blank lines so paragraph structure survives.
fn join_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(|ContentBlock::Text(t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{CallId, RawResult, Timestamp, ToolCall};

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
            entry(1, Payload::User(vec![ContentBlock::Text("hello there".into())])),
            entry(
                2,
                Payload::Assistant {
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
        let doc = project(&sample_log(), &LiveTail::default());

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
                DocumentNode::Turn { author: Author::Assistant, blocks } => Some(blocks),
                _ => None,
            })
            .expect("assistant turn");
        let exchanges = assistant_blocks
            .iter()
            .filter(|n| matches!(n, DocumentNode::TruncatedBlock { .. }))
            .count();
        assert_eq!(exchanges, 1, "one exchange, not separate call + result blocks");

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
        assert!(header_text.contains("bash"), "tool name in header: {header_text}");
        assert!(!header_text.contains("result #"), "no result label: {header_text}");
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
                    raw: RawResult { content: "RAW-NOISE".into(), is_error: false },
                    wire: WireState::Distilled("the distilled summary".into()),
                },
            ),
        ];
        let doc = project(&log, &LiveTail::default());
        let code: String = collect_code(&doc.nodes);
        assert!(code.contains("the distilled summary"), "shows distillate: {code}");
        assert!(!code.contains("RAW-NOISE"), "hides raw when distilled");
    }

    #[test]
    fn live_tail_renders_after_committed_entries_and_clears_on_commit() {
        let mut live = LiveTail::default();
        live.assistant.push_str("streaming…");
        live.push_tool_output("toolu_1", "partial out");

        let doc = project(&sample_log(), &live);
        let code = collect_code(&doc.nodes);
        assert!(code.contains("partial out"), "live tool output renders");

        // Committing the assistant entry clears the live assistant buffer.
        live.commit(&entry(
            4,
            Payload::Assistant {
                text: vec![ContentBlock::Text("done".into())],
                calls: vec![],
            },
        ));
        assert!(live.assistant.is_empty(), "assistant buffer cleared on commit");
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
        let mut state = InteractiveState::new(rx, submit_tx, approve_tx, rc, theme);
        state.document.document = project(&sample_log(), &LiveTail::default());

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
