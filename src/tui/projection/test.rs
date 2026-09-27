use super::*;
use crate::log::{Distilled, Keep, RawResult, Timestamp, ToolCall};

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
        source: None,
        payload,
    }
}

fn assistant(text: Vec<ContentBlock>, calls: Vec<ToolCall>) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(vec![], text, calls),
        upstream_failure: None,
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
            assistant(
                vec![ContentBlock::Text("**bold** plan".into())],
                vec![ToolCall::new(
                    "bash",
                    "toolu_1",
                    serde_json::json!({ "command": "echo hi" }),
                )],
            ),
        ),
        entry(
            3,
            Payload::tool_result(
                "toolu_1",
                RawResult::text("line one\nline two"),
                WireState::Raw,
            ),
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
    // no line budget reaches a tool result (docs/positions.md).
    let body: String = (1..=400).map(|n| format!("line {n}\n")).collect();
    let log = vec![
        entry(
            1,
            assistant(
                vec![],
                vec![ToolCall::new(
                    "bash",
                    "toolu_1",
                    serde_json::json!({ "command": "seq 400" }),
                )],
            ),
        ),
        entry(
            2,
            Payload::tool_result("toolu_1", RawResult::text(body.clone()), WireState::Raw),
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
            assistant(
                vec![],
                vec![ToolCall::new(
                    "bash",
                    "toolu_1",
                    serde_json::json!({ "command": "false" }),
                )],
            ),
        ),
        entry(
            2,
            Payload::tool_result("toolu_1", RawResult::error("boom"), WireState::Raw),
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
            assistant(
                vec![],
                vec![ToolCall::new(
                    "bash",
                    "toolu_9",
                    serde_json::json!({ "command": "noise" }),
                )],
            ),
        ),
        entry(
            5,
            Payload::tool_result(
                "toolu_9",
                RawResult::text("RAW-NOISE\nthe kept line"),
                WireState::from(
                    Distilled::new(Keep::Ranges(vec![(2, 2)])).with_note("the distilled summary"),
                ),
            ),
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
            assistant(
                vec![],
                vec![ToolCall::new(
                    "bash",
                    "toolu_9",
                    serde_json::json!({ "command": "noise" }),
                )],
            ),
        ),
        entry(
            5,
            Payload::tool_result(
                "toolu_9",
                RawResult::text("RAW-NOISE\nthe kept line"),
                WireState::from(
                    Distilled::new(Keep::Ranges(vec![(2, 2)])).with_note("the distilled summary"),
                ),
            ),
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
        assistant(vec![ContentBlock::Text("done".into())], vec![]),
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
    let (senders, _receivers) = crate::tui::senders::Senders::channels();
    let mut state = InteractiveState::new(rx, senders, rc, theme);
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

/// A result the control channel truncated says so in its header, so a body that
/// stops mid-output is not read as the whole of what the tool returned
/// (docs/control-api.md). The tag is the source — the TUI never measures the string
/// itself, which is only ever the prefix it was sent.
#[test]
fn an_elided_result_names_the_elision_in_its_header() {
    let mut log = sample_log();
    let Payload::ToolResult { raw, .. } = &mut log[2].payload else {
        panic!("the sample's third entry is the result");
    };
    raw.content = "x".repeat(16 * 1024);
    raw.content_elided = Some(crate::log::ContentElision {
        kept_bytes: 16 * 1024,
        total_bytes: 256 * 1024,
    });

    let doc = project(
        &log,
        &LiveTail::default(),
        &ViewState::default(),
        Gate::default(),
        &[],
    );
    let header = header_of(&doc, EntryKind::ToolResult);
    assert!(
        header.contains("16 KiB of 256 KiB shown"),
        "the header names what was kept and what exists: {header}"
    );

    // A whole result carries no marker at all.
    let plain = project(
        &sample_log(),
        &LiveTail::default(),
        &ViewState::default(),
        Gate::default(),
        &[],
    );
    assert_eq!(header_of(&plain, EntryKind::ToolResult), "2 lines");
}

/// The kickoff is part of the session's opening, like the preamble: it is not
/// drawn as a turn, so the agent's greeting is the first thing in the transcript
/// (docs/context.md). Any other sourced turn still renders.
#[test]
fn the_kickoff_turn_is_not_drawn() {
    let mut kickoff = entry(
        0,
        Payload::User(vec![ContentBlock::Text("greet them".into())]),
    );
    kickoff.source = Some(crate::frontend::KICKOFF_SOURCE.to_string());
    let mut sourced = entry(
        2,
        Payload::User(vec![ContentBlock::Text("from web".into())]),
    );
    sourced.source = Some("web".to_string());
    let log = vec![
        kickoff,
        entry(
            1,
            assistant(vec![ContentBlock::Text("Welcome".into())], vec![]),
        ),
        sourced,
    ];
    let doc = project(
        &log,
        &LiveTail::default(),
        &ViewState::default(),
        Gate::default(),
        &[],
    );
    assert_eq!(
        kinds(&doc),
        vec![EntryKind::AssistantMessage, EntryKind::UserMessage]
    );
}

/// A hand-off brief is the predecessor speaking (docs/tools.md): drawn as the
/// harness's note of the hand-off with the brief under it, never as the operator's
/// turn.
#[test]
fn a_handoff_brief_is_drawn_as_the_predecessors_note() {
    let mut brief = entry(
        0,
        Payload::User(vec![ContentBlock::Text("carry on".into())]),
    );
    brief.source = Some(crate::frontend::handoff_source("old-leaf"));
    let doc = project(
        &[brief],
        &LiveTail::default(),
        &ViewState::default(),
        Gate::default(),
        &[],
    );
    assert_eq!(kinds(&doc), vec![EntryKind::Annotation]);
}
