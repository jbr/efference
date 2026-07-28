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
        Gate::default(),
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
        Gate::default(),
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
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
    assert_eq!(exchange_level(&doc), TruncationLevel::Brief);

    // Expanding persists in the view.
    view.toggle_expanded("toolu_1");
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
    assert_eq!(exchange_level(&doc), TruncationLevel::Full);

    // A second projection (the streamed-token rebuild) keeps it expanded.
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
    assert_eq!(
        exchange_level(&doc),
        TruncationLevel::Full,
        "expand survived reprojection"
    );

    // Toggling back collapses, and that persists too.
    view.toggle_expanded("toolu_1");
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
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
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
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
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
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

    let doc = project(
        &sample_log(),
        &live,
        &ViewState::default(),
        Gate::default(),
        &[],
    );
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
        Gate::default(),
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
