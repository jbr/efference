use super::*;
use crate::log::{CallId, Keep, RawResult, Timestamp, ToolCall};

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
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
                upstream_failure: None,
            },
        ),
        entry(
            3,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    images: Vec::new(),
                    content: "line one\nline two".into(),
                    is_error: false,
                },
                wire: WireState::Raw,
            },
        ),
    ]
}

/// The kinds of the document's top-level entries, in order.
fn kinds(doc: &Document<'_>) -> Vec<EntryKind> {
    doc.nodes
        .iter()
        .filter_map(|n| match n {
            DocumentNode::Entry { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

/// The header text of the first entry of `kind`.
fn header_of(doc: &Document<'_>, want: EntryKind) -> String {
    doc.nodes
        .iter()
        .find_map(|n| match n {
            DocumentNode::Entry {
                kind,
                header: Some(h),
                ..
            } if *kind == want => Some(h.iter().map(|s| s.text.as_ref()).collect()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn projects_a_flat_sequence_of_entries() {
    let doc = project(
        &sample_log(),
        &LiveTail::default(),
        &ViewState::default(),
        Gate::default(),
        &[],
    );

    // The transcript is flat: one entry per top-level thing on the wire, with the
    // call and its result as *separate* entries (they are separate wire blocks, and
    // a result is not part of the call's content — docs/tui.md).
    assert_eq!(
        kinds(&doc),
        vec![
            EntryKind::UserMessage,
            EntryKind::AssistantMessage,
            EntryKind::ToolCall,
            EntryKind::ToolResult,
        ]
    );

    // The call header names the tool, with no role/seq label.
    let header = header_of(&doc, EntryKind::ToolCall);
    assert!(header.contains("bash"), "tool name in header: {header}");
    assert!(!header.contains("result #"), "no result label: {header}");
    assert!(!header.contains("wants to run"), "call already ran");

    // The result entry says how much came back, and holds it.
    assert_eq!(header_of(&doc, EntryKind::ToolResult), "2 lines");
    let code = collect_code(&doc.nodes);
    assert!(code.contains("line one"), "result body present: {code}");
}

#[test]
fn a_long_result_renders_whole() {
    // The position this unit exists for: the operator sees what the agent sees, so
    // no line budget reaches a tool result (docs/positions.md). Previously a body
    // over 8 lines was clipped to its first 8 by default.
    let body: String = (1..=400).map(|n| format!("line {n}\n")).collect();
    let log = vec![
        entry(
            1,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "seq 400" }),
                }],
                upstream_failure: None,
            },
        ),
        entry(
            2,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    images: Vec::new(),
                    content: body.clone(),
                    is_error: false,
                },
                wire: WireState::Raw,
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
    let code = collect_code(&doc.nodes);
    for n in [1, 9, 200, 400] {
        assert!(code.contains(&format!("line {n}\n")), "line {n} present");
    }
}

#[test]
fn a_failed_result_is_marked_on_the_result_not_the_call() {
    // The call is a call regardless of how it went; `✗` belongs to the entry that
    // actually failed, now that the two are separate.
    let log = vec![
        entry(
            1,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "false" }),
                }],
                upstream_failure: None,
            },
        ),
        entry(
            2,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    images: Vec::new(),
                    content: "boom".into(),
                    is_error: true,
                },
                wire: WireState::Raw,
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
    let marker = |want: EntryKind| {
        doc.nodes
            .iter()
            .find_map(|n| match n {
                DocumentNode::Entry { kind, marker, .. } if *kind == want => {
                    Some(marker.as_ref()?.text.to_string())
                }
                _ => None,
            })
            .unwrap_or_default()
    };
    assert_eq!(marker(EntryKind::ToolCall), "❯ ");
    assert_eq!(marker(EntryKind::ToolResult), "✗ ");
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
                upstream_failure: None,
            },
        ),
        entry(
            5,
            Payload::ToolResult {
                call_id: CallId("toolu_9".into()),
                raw: RawResult {
                    images: Vec::new(),
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
        code.contains("the kept line"),
        "shows the kept line: {code}"
    );
    assert!(
        !code.contains("RAW-NOISE"),
        "hides the dropped raw line: {code}"
    );

    // The distiller's note is *about* the result, so it is its own annotation entry
    // beside it rather than a line inside the result's body (docs/tui.md).
    assert!(
        !code.contains("the distilled summary"),
        "the note is not part of the result's content: {code}"
    );
    let annotations: String = doc
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n,
                DocumentNode::Entry {
                    kind: EntryKind::Annotation,
                    ..
                }
            )
        })
        .map(|n| collect_text(std::slice::from_ref(n)))
        .collect();
    assert!(
        annotations.contains("the distilled summary"),
        "the note renders as its own annotation: {annotations}"
    );
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
                upstream_failure: None,
            },
        ),
        entry(
            5,
            Payload::ToolResult {
                call_id: CallId("toolu_9".into()),
                raw: RawResult {
                    images: Vec::new(),
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
        !header_of(&doc, EntryKind::ToolResult).contains("raw"),
        "no raw marker by default"
    );

    // Flip to raw: the full raw content, and the header flags the override.
    view.toggle_raw("toolu_9");
    let doc = project(&log, &LiveTail::default(), &view, Gate::default(), &[]);
    let code = collect_code(&doc.nodes);
    assert!(code.contains("RAW-NOISE"), "full raw now shown: {code}");
    assert!(
        header_of(&doc, EntryKind::ToolResult).contains("raw"),
        "the result header marks the raw view"
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
    assert!(
        collect_text(&doc.nodes).contains("partial out"),
        "live tool output renders"
    );

    // Committing the assistant entry clears the live assistant buffer.
    live.commit(&entry(
        4,
        Payload::Assistant {
            thinking: vec![],
            text: vec![ContentBlock::Text("done".into())],
            calls: vec![],
            upstream_failure: None,
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
    let (sidebar_tx, _sidebar_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = InteractiveState::new(
        rx,
        submit_tx,
        approve_tx,
        pace_tx,
        interrupt_tx,
        sidebar_tx,
        rc,
        theme,
    );
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

/// All paragraph/header span text in the tree — for content that isn't a code
/// block (the live tail's dimmed lines, entry headers).
fn collect_text(nodes: &[DocumentNode<'_>]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            DocumentNode::Paragraph { spans } => {
                for span in spans {
                    out.push_str(&span.text);
                }
                out.push('\n');
            }
            DocumentNode::Lines { lines } => {
                for spans in lines {
                    for span in spans {
                        out.push_str(&span.text);
                    }
                    out.push('\n');
                }
            }
            DocumentNode::Entry { header, blocks, .. } => {
                for span in header.iter().flatten() {
                    out.push_str(&span.text);
                }
                out.push('\n');
                out.push_str(&collect_text(blocks));
            }
            _ => {}
        }
    }
    out
}

fn collect_code(nodes: &[DocumentNode<'_>]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            DocumentNode::CodeBlock { code, .. } => {
                out.push_str(code);
                out.push('\n');
            }
            // A distilled result's kept lines (docs/tui.md) — content like any other
            // code body, just carrying its original line numbers.
            DocumentNode::NumberedCode { lines, .. } => {
                for (_, text) in lines {
                    out.push_str(text);
                    out.push('\n');
                }
            }
            DocumentNode::Entry { blocks, .. } => out.push_str(&collect_code(blocks)),
            _ => {}
        }
    }
    out
}
