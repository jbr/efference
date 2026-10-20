//! Tool-result rendering: whole results, triaged verdicts, and errors.

use super::*;
use crate::log::Triaged;

/// The baseline for how a tool call and its result render: the call's command as a
/// shell block, the result whole under its own status header.
#[test]
fn transcript_tool_io() {
    let mut play = Screenplay::new(80, 30);
    play.user("Show me the project function signature.");
    play.assistant(
        "On it.",
        vec![ToolCall::new(
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
        vec![ToolCall::new(
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
        vec![ToolCall::new(
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
        "…and so is its tail, which must not be clipped at the border:\n{frame}"
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
        vec![ToolCall::new(
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

/// A `read_image` call shows what it asked for and at what resolution: the path
/// inline, `long_edge` beside it when present — image tokens scale with area
/// (docs/tools.md), so the requested size is part of what the call did. Without its
/// own arm the call falls through to the JSON envelope.
#[test]
fn transcript_read_image_call() {
    let mut play = Screenplay::new(80, 22);
    play.user("Look at the mock, then zoom into the header.");
    play.assistant(
        "Reading both.",
        vec![
            ToolCall::new("read_image", "toolu_i1", json!({ "path": "mock.png" })),
            ToolCall::new(
                "read_image",
                "toolu_i2",
                json!({ "path": "mock-header.png", "long_edge": 2576 }),
            ),
        ],
    );
    play.tool_result(
        "toolu_i1",
        "1200×1140 image/png, 1763 visual tokens",
        false,
        WireState::Raw,
    );
    play.tool_result(
        "toolu_i2",
        "2576×2449 image/png, 8092 visual tokens",
        false,
        WireState::Raw,
    );

    let frame = play.render();
    play.snapshot("transcript_read_image_call");
    assert!(
        !frame.contains("\"path\":"),
        "the call renders as a signature, not a JSON envelope:\n{frame}"
    );
    assert!(
        frame.contains("read_image(\"mock.png\"")
            && frame.contains("read_image(\"mock-header.png\""),
        "each path reads positionally on its call signature:\n{frame}"
    );
    assert!(
        frame.contains("long_edge: 2576"),
        "requested resolution beside the path when present:\n{frame}"
    );
}

/// A triaged tool result — the `WireState::Triaged` path, where the
/// transcript shows the curated summary in place of the raw output.
#[test]
fn transcript_triaged_result() {
    let mut play = Screenplay::new(80, 24);
    play.user("Summarize the dependency tree.");
    play.assistant(
        "Pulling the tree, then I'll triage it.",
        vec![ToolCall::new(
            "bash",
            "toolu_03",
            json!({ "command": "cargo tree" }),
        )],
    );
    play.tool_result(
        "toolu_03",
        "efference v0.1.0\n├── anyhow v1.0.86\n├── clap v4.5.4\n│   ├── …600 more lines…",
        false,
        WireState::from(Triaged::new(Keep::None).with_note(
            "12 direct deps; heaviest subtrees are tokio and trillium. Nothing outdated.",
        )),
    );
    play.snapshot("transcript_triaged_result");
}

/// Triage's request for a better intent renders as a second annotation after the
/// note, under the same `⟢` sign with its `re your intent:` lead — rare, and shown by
/// default because "triage couldn't tell what you wanted" is useful to a human on its
/// own (docs/triage.md).
#[test]
fn transcript_triaged_result_shows_intent_feedback() {
    let mut play = Screenplay::new(80, 20);
    play.user("Look around.");
    play.assistant(
        "",
        vec![
            ToolCall::new("bash", "toolu_if1", json!({ "command": "ls" }))
                .with_intent("list the directory"),
        ],
    );
    play.tool_result(
        "toolu_if1",
        "Cargo.toml
src
docs",
        false,
        WireState::from(
            Triaged::new(Keep::All)
                .with_note("3 entries: Cargo.toml, src, docs")
                .with_intent_feedback(
                    "it said what the call does, not what you needed; kept everything",
                ),
        ),
    );
    play.snapshot("transcript_triaged_result_with_intent_feedback");
}

/// When triage kept every line, the header presents the result like any whole
/// one — `N lines`, no `kept n of n` framing. "Kept everything" says only that
/// triage ran; nothing on screen may imply the operator is seeing less than
/// the model did, and here they are not.
#[test]
fn transcript_complete_selection_shows_a_plain_size() {
    let mut play = Screenplay::new(80, 22);
    play.user("Tail the log.");
    play.assistant(
        "",
        vec![ToolCall::new(
            "bash",
            "toolu_k1",
            json!({ "command": "tail -n 3 e.log" }),
        )],
    );
    play.tool_result(
        "toolu_k1",
        "line one\nline two\nline three",
        false,
        WireState::from(Triaged::new(Keep::All)),
    );

    let frame = play.render();
    play.snapshot("transcript_complete_selection_shows_a_plain_size");
    assert!(
        frame.contains("result") && !frame.contains("result ("),
        "a kept-everything result is a bare `result`, like any whole one:\n{frame}"
    );
    assert!(
        !frame.contains("of 3"),
        "no kept-n-of-n noise when nothing was dropped:\n{frame}"
    );
}

/// A triaged result shows the **triaged** lines, for every tool — the default
/// view is the agent's view (docs/tui.md). Triage kept only the prose line here, so
/// the diff section is gone and what survived renders numbered.
#[test]
fn a_partial_selection_of_an_edit_shows_the_triaged_lines() {
    let mut play = Screenplay::new(80, 24);
    play.user("Add the line.");
    play.assistant(
        "Editing.",
        vec![ToolCall::new(
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
         \x20- Interactive TUI (`efference`).",
        false,
        WireState::from(
            Triaged::new(Keep::Ranges(vec![(1, 1)]))
                .with_note("inserted the read line-range entry"),
        ),
    );

    let frame = play.render();
    play.snapshot("transcript_partial_selection_of_an_edit");
    assert!(
        !frame.contains("+- `read` tool line-range support"),
        "the dropped diff lines are not shown — the agent doesn't have them:\n{frame}"
    );
    assert!(
        frame.contains("kept 1 of 7 lines"),
        "the header says how much of the output survived:\n{frame}"
    );
    assert!(
        frame.contains("inserted the read line-range entry"),
        "the triager's note still shows:\n{frame}"
    );
}

/// A partly-triaged edit whose *diff lines* survived keeps the colored diff: the
/// `===DIFF===` parse runs over the whole result and the kept-line filter applies to
/// the rows it produced, so the tool's rendering survives triage instead of falling
/// back to plain text (docs/tui.md).
#[test]
fn a_partly_kept_diff_is_still_a_diff() {
    let mut play = Screenplay::new(80, 24);
    play.user("Add the line.");
    play.assistant(
        "Editing.",
        vec![ToolCall::new(
            "edit",
            "toolu_e4",
            json!({ "file_path": "CHANGELOG.md", "operation": "insert_after" }),
        )],
    );
    play.tool_result(
        "toolu_e4",
        "Applied insert after — the file has been updated.\n\
         \n\
         ===DIFF===\n\
         @@ -12,3 +12,4 @@\n\
         \x20- In-process MCP tools (fs-mcp, semantic-edit-mcp).\n\
         +- `read` tool line-range support in `fs-mcp`.\n\
         \x20- Interactive TUI (`efference`).",
        false,
        WireState::from(Triaged::new(Keep::Ranges(vec![(4, 6)]))),
    );

    let frame = play.render();
    play.snapshot("transcript_partly_kept_diff");
    assert!(
        frame.contains("+- `read` tool line-range support"),
        "the kept diff lines render as diff lines:\n{frame}"
    );
    assert!(
        !frame.contains("Interactive TUI"),
        "…and the dropped one doesn't:\n{frame}"
    );
    assert!(
        !frame.contains("Applied insert after"),
        "…nor does the dropped prose above the marker:\n{frame}"
    );
}

/// A selection that dropped nothing is the same case, and the one that surfaced it:
/// the fork returns `retain(all)` more often than not, and that was turning an
/// edit's colored diff into verbatim text with a bare `===DIFF===` line sitting in
/// the middle of it. Nothing was dropped here, so there is no divergence to state.
#[test]
fn a_complete_selection_keeps_its_per_tool_rendering() {
    let mut play = Screenplay::new(80, 22);
    play.user("Add the line.");
    play.assistant(
        "Editing.",
        vec![ToolCall::new(
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
         \x20- Interactive TUI (`efference`).",
        false,
        WireState::from(Triaged::new(Keep::All)),
    );

    let frame = play.render();
    play.snapshot("transcript_complete_selection_of_an_edit");
    assert!(
        !frame.contains("===DIFF==="),
        "the marker is consumed by the diff renderer, not printed:\n{frame}"
    );
    assert!(
        frame.contains("+- `read` tool line-range support"),
        "the added line renders as a diff line:\n{frame}"
    );
    // The call signature names the file, positionally, and what was done to it.
    assert!(
        frame.contains(r#""CHANGELOG.md","#) && frame.contains(r#"operation: "insert_after""#),
        "the signature names the file and the operation:\n{frame}"
    );
}

/// A selection that keeps **non-contiguous** ranges renders them as numbered
/// lines, with the gaps stated by the jump in the gutter rather than by a
/// `@@ lines a-b @@` header (docs/tui.md). The wire still gets the headers — it has
/// only text to say it with; a terminal has a gutter.
#[test]
fn transcript_triaged_keeps_numbered_lines() {
    let mut play = Screenplay::new(80, 24);
    play.user("Which tests failed?");
    play.assistant(
        "Running them.",
        vec![ToolCall::new(
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
        WireState::from(
            Triaged::new(Keep::Ranges(vec![(7, 7), (22, 23)]))
                .with_note("2 failures, both in the tui wrap/gutter work."),
        ),
    );

    let frame = play.render();
    play.snapshot("transcript_triaged_keeps_numbered_lines");
    assert!(
        !frame.contains("@@"),
        "no hunk headers in the TUI form:\n{frame}"
    );
    assert!(
        frame.contains("kept 3 of 40 lines"),
        "the header says how much of the output survived:\n{frame}"
    );
}

/// The same triaged result after flipping it to raw (`r`, docs/tui.md): the
/// header gains a `· raw` marker and the body shows the full raw output the
/// selection replaced — the human's escape hatch back to what the tool actually
/// returned, a pure client-side re-render (both forms are already local).
#[test]
fn transcript_raw_view_of_triaged() {
    let mut play = Screenplay::new(80, 24);
    play.user("Summarize the dependency tree.");
    play.assistant(
        "Pulling the tree, then I'll triage it.",
        vec![ToolCall::new(
            "bash",
            "toolu_03",
            json!({ "command": "cargo tree" }),
        )],
    );
    play.tool_result(
        "toolu_03",
        "efference v0.1.0\n├── anyhow v1.0.86\n├── clap v4.5.4\n└── tokio v1.38.0",
        false,
        WireState::from(
            Triaged::new(Keep::None)
                .with_note("12 direct deps; tokio + trillium are the heaviest."),
        ),
    );
    play.show_raw("toolu_03");
    play.snapshot("transcript_raw_view_of_triaged");
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
        vec![ToolCall::new(
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
        vec![ToolCall::new(
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
        vec![ToolCall::new(
            "list",
            "toolu_r3",
            json!({ "path": "src/tui" }),
        )],
    );
    play.tool_result(
        "toolu_r3",
        "All paths relative to src/tui:\n\n\ncomposer.rs\ndocument.rs\nprojection.rs",
        false,
        WireState::Raw,
    );
    play.snapshot("transcript_file_tools");
}

/// A triaged `read` keeps its per-tool rendering too — sentinel framing dropped,
/// highlighted by extension — over just the kept lines, in a gutter numbered by the
/// *result's* line numbers (the coordinate system the verdict was expressed in, so
/// the operator's view and the agent's name the same lines).
#[test]
fn a_partly_kept_read_keeps_its_highlighting() {
    let mut play = Screenplay::new(80, 24);
    play.user("Open state.rs.");
    play.assistant(
        "Reading the file:",
        vec![ToolCall::new(
            "read",
            "toolu_r4",
            json!({ "paths": ["src/tui/state.rs"] }),
        )],
    );
    play.tool_result(
        "toolu_r4",
        "==MK7 BEGIN /Users/jbr/code/efference/src/tui/state.rs, LENGTH: 96 MK7==\n\
             pub struct InteractiveState<'a> {\n\
             \x20   pub document: DocumentState<'a>,\n\
             \x20   pub viewport: ViewportState,\n\
             }\n\
             ==MK7 END /Users/jbr/code/efference/src/tui/state.rs MK7==",
        false,
        WireState::from(
            Triaged::new(Keep::Ranges(vec![(2, 3)]))
                .with_note("the struct head and its first field"),
        ),
    );

    let frame = play.render();
    play.snapshot("transcript_partly_kept_read");
    assert!(
        frame.contains("pub struct InteractiveState"),
        "the kept lines are on screen:\n{frame}"
    );
    assert!(
        !frame.contains("pub viewport"),
        "…and the dropped ones are not:\n{frame}"
    );
    assert!(
        !frame.contains("MK7"),
        "the sentinel framing is still consumed, not printed:\n{frame}"
    );
}

/// A failing tool call — the error result path (struck-through label, the
/// `error #seq` tag) so error styling has a baseline.
#[test]
fn transcript_error_result() {
    let mut play = Screenplay::new(80, 24);
    play.user("Run the test suite.");
    play.assistant(
        "Running the full suite now.",
        vec![ToolCall::new(
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

/// A `WireState::TriageFailed` result (docs/triage.md) surfaces to the
/// operator like a failed call: the error prompt shows even though the tool itself
/// succeeded (`is_error: false` on the raw), the withheld message is on screen, and
/// the untriaged raw is not.
#[test]
fn a_triage_failed_result_reads_as_a_failed_call() {
    let mut play = Screenplay::new(80, 16);
    play.user("Run it.");
    play.assistant(
        "",
        vec![ToolCall::new(
            "bash",
            "toolu_tf",
            json!({ "command": "ls" }),
        )],
    );
    play.tool_result(
        "toolu_tf",
        "ten thousand lines of untriaged output",
        false,
        WireState::TriageFailed {
            why: "triage called no tool (attempt 3 of 3)".into(),
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

/// A recovered in-fork retry is not silent (docs/triage.md): the `Triage`
/// entry's `retries` breadcrumbs render as annotations beside the result, so the
/// operator sees the compliance signal even though the gate ultimately succeeded.
#[test]
fn a_recovered_retry_surfaces_as_an_annotation() {
    use crate::log::{Payload, Triage};
    let mut play = Screenplay::new(80, 16);
    play.user("Run it.");
    play.assistant(
        "",
        vec![ToolCall::new(
            "bash",
            "toolu_rt",
            json!({ "command": "ls" }),
        )],
    );
    play.tool_result("toolu_rt", "file.txt", false, WireState::Raw);
    play.commit(Payload::Triage(Triage {
        for_result: Some(3),
        origin: crate::log::Origin::Model,
        raw_keep: "all".into(),
        inbox: Vec::new(),
        surfaced: Vec::new(),
        retries: vec!["attempt 1 of 3: triage called bash instead of retain".into()],
        thinking: Vec::new(),
        shown_through: None,
    }));
    assert!(
        play.find("⚠ triage retry").is_some(),
        "the non-compliant attempt is on screen despite the recovery"
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
        vec![ToolCall::new(
            "bash",
            "toolu_07",
            json!({ "command": "false" }),
        )],
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
