//! Tool-result rendering: expansion, collapse, distilled verdicts, and errors.

use super::*;

/// The same coding turn, but with every collapsed block expanded — so the
/// baseline shows how tool-call JSON input and raw tool output actually render
/// (a fenced code block inside the outdented `╰─` border), not just the
/// collapsed `[...]` snippet. This is the form behind every `⏎`-to-expand.
#[test]
fn transcript_expanded_tool_io() {
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
    play.expand_all();
    play.snapshot("transcript_expanded_tool_io");
}

/// A result longer than the collapse threshold: by default it shows the first
/// few lines under a `▶` disclosure triangle with a `(+N lines)` hint, rather
/// than hiding everything. This is the accordion's collapsed state.
#[test]
fn transcript_long_result_collapsed() {
    let mut play = Screenplay::new(80, 26);
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
    play.snapshot("transcript_long_result_collapsed");
}

/// The same long result after activating its disclosure triangle: `▼` and the
/// full body, no `(+N lines)` hint. Drives the real `ExpandBlock` action.
#[test]
fn transcript_long_result_expanded() {
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
    play.expand_all();
    play.snapshot("transcript_long_result_expanded");
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
