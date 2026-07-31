//! Tool-result rendering: whole results, distilled verdicts, and errors.

use super::*;

/// The baseline for how a tool call and its result render: the call's command as a
/// shell block, the result whole under its own status header.
#[test]
fn transcript_tool_io() {
    let mut play = Screenplay::new(80, 30);
    play.user("Show me the project function signature.");
    play.assistant(
        "On it.",
        vec![tool_call(
            "bash",
            "toolu_01",
            json!({ "command": "grep -n 'fn project' src/project.rs" }),
        )],
    );
    play.tool_result(
        "toolu_01",
        "12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {\n\
             88:    fn project_coalesces_tool_results() {",
        false,
        WireState::Raw,
    );
    play.snapshot("transcript_tool_io");
}

/// A result far longer than the old 8-line budget renders **whole** — the default
/// view is the whole thing, because the operator can't perceive what the agent is
/// dealing with through a summary of it (docs/positions.md).
#[test]
fn transcript_long_result() {
    let mut play = Screenplay::new(80, 32);
    play.user("List the tui module.");
    play.assistant(
        "Here's the directory.",
        vec![tool_call(
            "bash",
            "toolu_05",
            json!({ "command": "ls src/tui" }),
        )],
    );
    let listing = (1..=20)
        .map(|n| format!("render_module_{n:02}.rs"))
        .collect::<Vec<_>>()
        .join("\n");
    play.tool_result("toolu_05", &listing, false, WireState::Raw);
    play.snapshot("transcript_long_result");
}

/// A result line wider than the code box **wraps inside it**, rather than running
/// past the right border and being clipped there. Untruncated output is worth
/// little if the long lines are cut at the frame — and a stack-trace or compiler
/// error is exactly the shape that overflows (docs/tui.md).
#[test]
fn transcript_long_line_wraps_inside_the_box() {
    let mut play = Screenplay::new(80, 20);
    play.user("Why did it fail?");
    play.assistant(
        "Reading the failure.",
        vec![tool_call(
            "bash",
            "toolu_09",
            json!({ "command": "cargo test --lib" }),
        )],
    );
    play.tool_result(
        "toolu_09",
        "thread 'tui::projection::test::projects_a_flat_sequence_of_entries' panicked at \
         src/tui/projection/test.rs:88:5: assertion `left == right` failed: one entry per \
         wire block\nshort tail line",
        true,
        WireState::Raw,
    );
    // Nothing may sit right of the border column, on any row.
    let frame = play.render();
    play.snapshot("transcript_long_line_wraps_inside_the_box");
    assert!(
        frame.contains("panicked at"),
        "the head of the long line is shown"
    );
    assert!(
        frame.contains("wire block"),
        "…and so is its tail, which used to be clipped at the border:\n{frame}"
    );
}

/// A `write` call shows the file it is writing *as a file*: the path inline, the
/// contents as a code block highlighted by extension. The JSON envelope rendered
/// `contents` as one escaped string with literal `\n`s — the least readable form of
/// the only thing worth reading in the call.
#[test]
fn transcript_write_call() {
    let mut play = Screenplay::new(80, 22);
    play.user("Write a counter script.");
    play.assistant(
        "Writing it now.",
        vec![tool_call(
            "write",
            "toolu_w1",
            json!({
                "path": "/tmp/counter.sh",
                "contents": "#!/bin/bash\n\n# Print a counter once per second\n\
                             for i in {1..60}; do\n  echo \"Counter: $i\"\n  sleep 1\ndone\n",
            }),
        )],
    );
    play.tool_result(
        "toolu_w1",
        "wrote 7 lines to /tmp/counter.sh",
        false,
        WireState::Raw,
    );

    let frame = play.render();
    play.snapshot("transcript_write_call");
    assert!(
        !frame.contains("\\n"),
        "contents render as lines, not as an escaped JSON string:\n{frame}"
    );
    assert!(
        frame.contains("❬bash❭"),
        "…highlighted by the path's extension:\n{frame}"
    );
}

/// A distilled tool result — the `WireState::Distilled` path, where the
/// transcript shows the curated summary in place of the raw output.
#[test]
fn transcript_distilled_result() {
    let mut play = Screenplay::new(80, 24);
    play.user("Summarize the dependency tree.");
    play.assistant(
        "Pulling the tree, then I'll distill it.",
        vec![tool_call(
            "bash",
            "toolu_03",
            json!({ "command": "cargo tree" }),
        )],
    );
    play.tool_result(
        "toolu_03",
        "efference v0.1.0\n├── anyhow v1.0.86\n├── clap v4.5.4\n│   ├── …600 more lines…",
        false,
        WireState::Distilled {
            keep: Keep::None,
            note: Some(
                "12 direct deps; heaviest subtrees are tokio and trillium. Nothing outdated."
                    .into(),
            ),
        },
    );
    play.snapshot("transcript_distilled_result");
}

/// A tool with its own representation renders the **full** output through it, even
/// when triage kept only part — a subset of a diff's lines is a mangled diff, not a
/// smaller one. Where the operator is therefore seeing more than the agent did,
/// that gap is stated rather than left to be inferred (docs/tui.md).
#[test]
fn a_partial_distillate_of_an_edit_still_renders_whole() {
    let mut play = Screenplay::new(80, 24);
    play.user("Add the line.");
    play.assistant(
        "Editing.",
        vec![tool_call(
            "edit",
            "toolu_e3",
            json!({ "file_path": "CHANGELOG.md", "operation": "insert_after" }),
        )],
    );
    play.tool_result(
        "toolu_e3",
        "Applied insert after — the file has been updated.\n\
         \n\
         ===DIFF===\n\
         @@ -12,3 +12,4 @@\n\
         \x20- In-process MCP tools (fs-mcp, semantic-edit-mcp).\n\
         +- `read` tool line-range support in `fs-mcp`.\n\
         \x20- Interactive TUI (`efference tui`).",
        false,
        WireState::Distilled {
            keep: Keep::Ranges(vec![(1, 1)]),
            note: Some("inserted the read line-range entry".into()),
        },
    );

    let frame = play.render();
    play.snapshot("transcript_partial_distillate_of_an_edit");
    assert!(
        frame.contains("+- `read` tool line-range support"),
        "the diff renders whole, not clipped to the kept range:\n{frame}"
    );
    assert!(
        frame.contains("inserted the read line-range entry"),
        "the distiller's note still shows:\n{frame}"
    );
    assert!(
        frame.contains("the agent's copy kept 1 of 7 lines"),
        "…and so does the fact that the agent saw less than this:\n{frame}"
    );
}

/// A distillate that dropped nothing is the same case, and the one that surfaced it:
/// the fork returns `retain(all)` more often than not, and that was turning an
/// edit's colored diff into verbatim text with a bare `===DIFF===` line sitting in
/// the middle of it. Nothing was dropped here, so there is no divergence to state.
#[test]
fn a_complete_distillate_keeps_its_per_tool_rendering() {
    let mut play = Screenplay::new(80, 22);
    play.user("Add the line.");
    play.assistant(
        "Editing.",
        vec![tool_call(
            "edit",
            "toolu_e2",
            json!({
                "file_path": "CHANGELOG.md",
                "operation": "insert_after",
                "anchor": "- In-process MCP tools (fs-mcp, semantic-edit-mcp) reusing the enums.",
            }),
        )],
    );
    play.tool_result(
        "toolu_e2",
        "Applied insert after — the file has been updated.\n\
         \n\
         ===DIFF===\n\
         @@ -12,3 +12,4 @@\n\
         \x20- In-process MCP tools (fs-mcp, semantic-edit-mcp).\n\
         +- `read` tool line-range support in `fs-mcp`.\n\
         \x20- Interactive TUI (`efference tui`).",
        false,
        WireState::Distilled {
            keep: Keep::All,
            note: None,
        },
    );

    let frame = play.render();
    play.snapshot("transcript_complete_distillate_of_an_edit");
    assert!(
        !frame.contains("===DIFF==="),
        "the marker is consumed by the diff renderer, not printed:\n{frame}"
    );
    assert!(
        frame.contains("+- `read` tool line-range support"),
        "the added line renders as a diff line:\n{frame}"
    );
    // The call header names the file and what was done to it — not a fragment of
    // the anchor, which the diff below shows in full anyway.
    assert!(
        frame.contains("edit · CHANGELOG.md · insert_after"),
        "the header names the file and the operation:\n{frame}"
    );
}

/// A distillate that keeps **non-contiguous** ranges renders them as numbered
/// lines, with the gaps stated by the jump in the gutter rather than by a
/// `@@ lines a-b @@` header (docs/tui.md). The wire still gets the headers — it has
/// only text to say it with; a terminal has a gutter.
#[test]
fn transcript_distilled_keeps_numbered_lines() {
    let mut play = Screenplay::new(80, 24);
    play.user("Which tests failed?");
    play.assistant(
        "Running them.",
        vec![tool_call(
            "bash",
            "toolu_04",
            json!({ "command": "cargo test" }),
        )],
    );
    let output: String = (1..=40)
        .map(|n| match n {
            7 => "test tui::wrap::breaks_on_whitespace ... FAILED\n".to_string(),
            22 => "test tui::gutter::numbers_are_dimmed ... FAILED\n".to_string(),
            23 => "test result: FAILED. 38 passed; 2 failed\n".to_string(),
            other => format!("test case_{other:02} ... ok\n"),
        })
        .collect();
    play.tool_result(
        "toolu_04",
        &output,
        false,
        WireState::Distilled {
            keep: Keep::Ranges(vec![(7, 7), (22, 23)]),
            note: Some("2 failures, both in the tui wrap/gutter work.".into()),
        },
    );

    let frame = play.render();
    play.snapshot("transcript_distilled_keeps_numbered_lines");
    assert!(
        !frame.contains("@@"),
        "no hunk headers in the TUI form:\n{frame}"
    );
    assert!(
        frame.contains("kept 3 of 40 lines"),
        "the header says how much of the output survived:\n{frame}"
    );
}

/// The same distilled result after flipping it to raw (`r`, docs/tui.md): the
/// header gains a `· raw` marker and the body shows the full raw output the
/// distillate replaced — the human's escape hatch back to what the tool actually
/// returned, a pure client-side re-render (both forms are already local).
#[test]
fn transcript_raw_view_of_distilled() {
    let mut play = Screenplay::new(80, 24);
    play.user("Summarize the dependency tree.");
    play.assistant(
        "Pulling the tree, then I'll distill it.",
        vec![tool_call(
            "bash",
            "toolu_03",
            json!({ "command": "cargo tree" }),
        )],
    );
    play.tool_result(
        "toolu_03",
        "efference v0.1.0\n├── anyhow v1.0.86\n├── clap v4.5.4\n└── tokio v1.38.0",
        false,
        WireState::Distilled {
            keep: Keep::None,
            note: Some("12 direct deps; tokio + trillium are the heaviest.".into()),
        },
    );
    play.show_raw("toolu_03");
    play.snapshot("transcript_raw_view_of_distilled");
}

/// The file tools render tool-specifically: `read`/`search`/`list` calls show
/// their path/pattern as one concise line (not a JSON envelope), and a `read`
/// result drops its `==… BEGIN …==` sentinel framing and highlights by file type.
#[test]
fn transcript_file_tools() {
    let mut play = Screenplay::new(80, 40);
    play.user("Open state.rs, then search and list.");
    play.assistant(
        "Reading the file:",
        vec![tool_call(
            "read",
            "toolu_r1",
            json!({ "paths": ["src/tui/state.rs"] }),
        )],
    );
    play.tool_result(
        "toolu_r1",
        "==MK7 BEGIN /Users/jbr/code/efference/src/tui/state.rs, LENGTH: 96 MK7==\n\
             pub struct InteractiveState<'a> {\n\
             \x20   pub document: DocumentState<'a>,\n\
             \x20   pub viewport: ViewportState,\n\
             }\n\
             ==MK7 END /Users/jbr/code/efference/src/tui/state.rs MK7==",
        false,
        WireState::Raw,
    );
    play.assistant(
        "Now searching:",
        vec![tool_call(
            "search",
            "toolu_r2",
            json!({ "pattern": "fn project", "path": "src" }),
        )],
    );
    play.tool_result(
            "toolu_r2",
            "Found 1 match for pattern \"fn project\":\n\nsrc/project.rs:12: pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {",
            false,
            WireState::Raw,
        );
    play.assistant(
        "And listing:",
        vec![tool_call("list", "toolu_r3", json!({ "path": "src/tui" }))],
    );
    play.tool_result(
        "toolu_r3",
        "All paths relative to src/tui:\n\n\ncomposer.rs\ndocument.rs\nprojection.rs",
        false,
        WireState::Raw,
    );
    play.snapshot("transcript_file_tools");
}

/// A failing tool call — the error result path (struck-through label, the
/// `error #seq` tag) so error styling has a baseline.
#[test]
fn transcript_error_result() {
    let mut play = Screenplay::new(80, 24);
    play.user("Run the test suite.");
    play.assistant(
        "Running the full suite now.",
        vec![tool_call(
            "bash",
            "toolu_02",
            json!({ "command": "cargo test" }),
        )],
    );
    play.tool_result(
        "toolu_02",
        "error[E0599]: no method named `reproject` found for struct `InteractiveState`\n\
             error: could not compile `efference` (lib test) due to 1 previous error",
        true,
        WireState::Raw,
    );
    play.snapshot("transcript_error_result");
}

/// A `WireState::TriageFailed` result (docs/redaction-fork.md) surfaces to the
/// operator like a failed call: the error prompt shows even though the tool itself
/// succeeded (`is_error: false` on the raw), the withheld message is on screen, and
/// the untriaged raw is not.
#[test]
fn a_triage_failed_result_reads_as_a_failed_call() {
    let mut play = Screenplay::new(80, 16);
    play.user("Run it.");
    play.assistant(
        "",
        vec![tool_call("bash", "toolu_tf", json!({ "command": "ls" }))],
    );
    play.tool_result(
        "toolu_tf",
        "ten thousand lines of untriaged output",
        false,
        WireState::TriageFailed {
            why: "peek called no tool (attempt 3 of 3)".into(),
        },
    );
    assert!(
        play.find("✗").is_some(),
        "surfaced as an error despite the tool succeeding"
    );
    assert!(
        play.find("result withheld — triage failed").is_some(),
        "the withheld message is on screen"
    );
    assert!(
        play.find("ten thousand lines").is_none(),
        "the untriaged raw stays off the default view"
    );
}

/// A failed exchange reads as an error via a red `✗` prompt (replacing the
/// normal `❯`) — and explicitly NOT strikethrough (which now means deletion).
/// The character grid can't show color; `cell` asserts it.
#[test]
fn error_exchange_prompt_is_red() {
    let mut play = Screenplay::new(80, 16);
    play.user("Run it.");
    play.assistant(
        "",
        vec![tool_call("bash", "toolu_07", json!({ "command": "false" }))],
    );
    play.tool_result("toolu_07", "command failed", true, WireState::Raw);

    let (x, y) = play.find("✗").expect("error prompt is on screen");
    let style = play.cell(x, y);
    assert!(
        !style.has(Modifier::CROSSED_OUT),
        "an error is not a deletion — it shouldn't be struck through: {style:?}",
    );
    let (r, g, b) = rgb(style.fg);
    assert!(r > g && r > b, "error prompt should be red, got {style:?}");
}
