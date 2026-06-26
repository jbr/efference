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

use crate::log::{ContentBlock, LogEntry, MarkKind, Payload, RawResult, ToolCall, WireState};
use crate::tui::document::{
    Document, DocumentNode, ListItem, Span, SpanStyle, TruncationLevel,
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

/// Fold the log + live tail into a transcript document.
pub(crate) fn project(log: &[LogEntry], live: &LiveTail) -> Document<'static> {
    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();

    for entry in log {
        match &entry.payload {
            Payload::User(blocks) => push_user(&mut nodes, blocks),
            Payload::Assistant { text, calls } => push_assistant(&mut nodes, text, calls),
            Payload::ToolResult { raw, wire, .. } => {
                push_tool_result(&mut nodes, entry.seq, raw, wire)
            }
            Payload::Mark(mark) => push_mark(&mut nodes, &mark.kind, &mark.note),
        }
    }

    // The live tail renders after everything committed.
    if !live.assistant.is_empty() {
        nodes.push(label("assistant", SpanStyle::Keyword));
        nodes.extend(markdown(&live.assistant));
    }
    for (call_id, buf) in &live.tools {
        nodes.push(label(&format!("running {call_id}"), SpanStyle::FunctionName));
        nodes.push(code_block(None, buf));
    }

    Document::with_nodes(nodes)
}

/// A single styled role marker line (e.g. "▌ assistant").
fn label(text: &str, style: SpanStyle) -> DocumentNode<'static> {
    DocumentNode::Paragraph {
        spans: vec![Span {
            text: format!("▌ {text}").into(),
            style,
            action: None,
        }],
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

fn push_user(nodes: &mut Vec<DocumentNode<'static>>, blocks: &[ContentBlock]) {
    nodes.push(label("you", SpanStyle::Strong));
    let text = join_text(blocks);
    nodes.extend(markdown(&text));
}

fn push_assistant(nodes: &mut Vec<DocumentNode<'static>>, text: &[ContentBlock], calls: &[ToolCall]) {
    let prose = join_text(text);
    if !prose.trim().is_empty() {
        nodes.push(label("assistant", SpanStyle::Keyword));
        nodes.extend(markdown(&prose));
    }
    for call in calls {
        push_call(nodes, call);
    }
}

fn push_call(nodes: &mut Vec<DocumentNode<'static>>, call: &ToolCall) {
    nodes.push(label(&format!("{} (wants to run)", call.name), SpanStyle::FunctionName));
    let pretty = serde_json::to_string_pretty(&call.input).unwrap_or_else(|_| call.input.to_string());
    // The input is collapsible so a long argument blob doesn't dominate the view.
    nodes.push(DocumentNode::truncated_block(
        vec![code_block(Some("json"), &pretty)],
        TruncationLevel::Brief,
    ));
}

fn push_tool_result(
    nodes: &mut Vec<DocumentNode<'static>>,
    seq: u64,
    raw: &RawResult,
    wire: &WireState,
) {
    // Distillate-when-present, else raw — the same precedence as the wire
    // projection (`crate::project::tool_result_block`).
    let (content, marker_style) = match wire {
        WireState::Raw => (raw.content.clone(), SpanStyle::Comment),
        WireState::Distilled(s) => (s.clone(), SpanStyle::Emphasis),
        WireState::Elided => ("[elided]".to_string(), SpanStyle::Comment),
    };
    let style = if raw.is_error {
        SpanStyle::Strikethrough
    } else {
        marker_style
    };
    let tag = if raw.is_error { "error" } else { "result" };
    nodes.push(label(&format!("{tag} #{seq}"), style));
    // Long output collapses; short output shows in full (the renderer decides
    // based on the TruncationLevel hint).
    nodes.push(DocumentNode::truncated_block(
        vec![code_block(None, &content)],
        TruncationLevel::Brief,
    ));
}

fn push_mark(nodes: &mut Vec<DocumentNode<'static>>, kind: &MarkKind, note: &str) {
    nodes.push(DocumentNode::horizontal_rule());
    let MarkKind::Boundary { upto } = kind;
    nodes.push(DocumentNode::List {
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
    fn projects_each_payload_into_labeled_blocks() {
        let doc = project(&sample_log(), &LiveTail::default());

        // The role labels (▌ you / assistant / wants-to-run / result) all land,
        // and tool output / call input are collapsible TruncatedBlocks.
        let mut paragraph_text = String::new();
        let mut truncated = 0;
        for node in &doc.nodes {
            match node {
                DocumentNode::Paragraph { spans } => {
                    for s in spans {
                        paragraph_text.push_str(&s.text);
                        paragraph_text.push('\n');
                    }
                }
                DocumentNode::TruncatedBlock { .. } => truncated += 1,
                _ => {}
            }
        }
        assert!(paragraph_text.contains("▌ you"), "user label: {paragraph_text}");
        assert!(paragraph_text.contains("▌ assistant"), "assistant label");
        assert!(paragraph_text.contains("bash (wants to run)"), "tool-call label");
        assert!(paragraph_text.contains("result #3"), "tool-result label");
        // One collapsible block for the call input, one for the result output.
        assert_eq!(truncated, 2, "collapsible call-input + tool-output blocks");
    }

    #[test]
    fn distilled_wire_supersedes_raw() {
        let log = vec![entry(
            5,
            Payload::ToolResult {
                call_id: CallId("toolu_9".into()),
                raw: RawResult {
                    content: "RAW-NOISE".into(),
                    is_error: false,
                },
                wire: WireState::Distilled("the distilled summary".into()),
            },
        )];
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
        assert!(screen.contains("you"), "user turn on screen");
        assert!(screen.contains("plan"), "assistant prose on screen");
        // The tool result's label renders; its body sits in a collapsed
        // TruncatedBlock (the collapsible-node feature), so we assert the label,
        // not the hidden output.
        assert!(screen.contains("result"), "tool-result label on screen");
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
                _ => {}
            }
        }
        out
    }
}
