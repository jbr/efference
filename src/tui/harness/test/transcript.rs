//! Transcript rendering — one scenario per element type, plus the color assertions the character grid can't carry.

use super::*;

/// An edit tool's result renders as a diff: the `===DIFF===` preamble/tips are
/// stripped, and the `+`/`-`/context lines keep their prefixes for alignment.
/// (Color isn't in the grid — `diff_lines_are_colored` asserts that.)
#[test]
fn transcript_edit_diff() {
    edit_scenario(80, 32).snapshot("transcript_edit_diff");
}

/// The payoff for the cell-style tool: a `+` line is rendered green and a `-`
/// line red — which the character grid can't show. Asserted by channel
/// dominance so it holds across themes (the default theme's diff colors or the
/// built-in fallback).
#[test]
fn diff_lines_are_colored() {
    let mut play = edit_scenario(80, 32);

    let (ax, ay) = play
        .find("+        self.lines")
        .expect("added line on screen");
    let added = play.cell(ax + 1, ay); // a glyph past the '+' prefix
    let (rx, ry) = play
        .find("-        self.lines")
        .expect("removed line on screen");
    let removed = play.cell(rx + 1, ry);

    let (ar, ag, ab) = rgb(added.fg);
    assert!(
        ag > ar && ag > ab,
        "added line should be green, got {added:?}"
    );
    let (rr, rg, rb) = rgb(removed.fg);
    assert!(
        rr > rg && rr > rb,
        "removed line should be red, got {removed:?}"
    );
}

/// The bread-and-butter shape: a user question, an assistant turn with
/// markdown prose (bold, inline code, a bullet list) and a tool call, then the
/// tool's result. This is the canonical transcript baseline.
#[test]
fn transcript_coding_turn() {
    let mut play = Screenplay::new(80, 40);
    play.user("Why does `project()` rebuild the whole wire array every turn? Can we cache the committed prefix instead?");
    play.assistant(
        "Good question. The rebuild is **deliberate**, not an oversight:\n\n\
             - `project` is a pure fold over the log, so it's cheap to recompute\n\
             - caching a prefix would mean tracking invalidation across `WireState` flips\n\
             - the prompt cache already covers the *wire* cost — only the prefix bytes matter\n\n\
             Let me confirm the current signature before we touch it.",
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
    play.assistant(
        "Right — `project(&[LogEntry]) -> Vec<ApiMessage>` is the whole surface. \
             Caching would have to wrap *that*, which breaks byte-stability. I'd leave it.",
        vec![],
    );
    play.snapshot("transcript_coding_turn");
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

/// A bash call renders its `command` as a `bash` code block — true newlines and
/// shell escapes, not a `{"command": "…\n…"}` JSON envelope. A heredoc proves
/// the multiline payload survives as real lines.
#[test]
fn transcript_bash_command_block() {
    let mut play = Screenplay::new(80, 20);
    play.user("Write the version file.");
    play.assistant(
        "Writing it now.",
        vec![tool_call(
            "bash",
            "toolu_06",
            json!({ "command": "cat > VERSION <<'EOF'\n0.1.0\nbuild: nightly\nEOF" }),
        )],
    );
    play.tool_result("toolu_06", "", false, WireState::Raw);
    play.snapshot("transcript_bash_command_block");
}

/// GFM `~~strikethrough~~` renders as actual struck-through text (the `~~`
/// markers gone, `Modifier::CROSSED_OUT` applied) — neither of which the
/// character grid shows, so `cell` checks the modifier.
#[test]
fn markdown_strikethrough_is_crossed_out() {
    let mut play = Screenplay::new(60, 8);
    play.assistant("keep ~~gone~~ stay", vec![]);
    assert!(
        play.find("~~").is_none(),
        "the ~~ markers should be consumed"
    );
    let (x, y) = play.find("gone").expect("strikethrough text renders");
    assert!(
        play.cell(x, y).has(Modifier::CROSSED_OUT),
        "strikethrough text should be crossed out",
    );
}

/// Authorship is carried by color, not a word: the user's turn gets a blue
/// accent gutter (`┃`) in the left margin; the assistant has none. The grid
/// shows the glyph but not its color, so `cell` checks the accent.
#[test]
fn user_turn_has_a_blue_accent_gutter() {
    let mut play = Screenplay::new(60, 8);
    play.user("hello there");
    let (_, y) = play.find("hello there").expect("user prompt on screen");
    let gutter = play.cell(1, y); // the reserved left-margin gutter column
    assert_eq!(gutter.symbol, "┃", "user turn carries a gutter glyph");
    let (r, _g, b) = rgb(gutter.fg);
    assert!(
        b > r,
        "the user gutter should be a blue accent, got {gutter:?}"
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

/// A pending approval gate, rendered inline at the tail call rather than as a
/// bottom banner (docs/tui.md): the exchange shows a `⏸` marker and an
/// `awaiting your approval …` affordance where the result will land — the gate
/// reads as "after the call, before the result." Resolving it replaces the
/// affordance with the real (or error) result.
#[test]
fn transcript_pending_approval_gate() {
    let mut play = Screenplay::new(80, 14);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![tool_call(
            "bash",
            "toolu_1",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate("toolu_1", "bash", json!({ "command": "rm -rf ./build" }));

    // The `⏸` marker and the affordance lead-in carry the user-accent (blue)
    // color — "your turn" — which the character grid can't show.
    let (mx, my) = play.find("⏸").expect("pending marker on screen");
    let (r, _g, b) = rgb(play.cell(mx, my).fg);
    assert!(
        b > r,
        "the pending marker should be a blue accent, got ({r},{b})"
    );
    let (ax, ay) = play.find("awaiting").expect("affordance on screen");
    let (r, _g, b) = rgb(play.cell(ax, ay).fg);
    assert!(
        b > r,
        "the affordance lead-in should be a blue accent, got ({r},{b})"
    );

    play.snapshot("transcript_pending_approval_gate");
}

/// Redirecting a gate: the call stays in the transcript with a message *instead
/// of a result* (docs/tui.md). The loop clears the gate, pairs the orphaned
/// `tool_use` with an error result ("not run — the user replied…"), and the
/// human's words follow as a user turn — so the exchange reads `✗ bash …` over
/// the breadcrumb, then the steer.
#[test]
fn transcript_redirected_gate() {
    let mut play = Screenplay::new(80, 16);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![tool_call(
            "bash",
            "toolu_1",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate("toolu_1", "bash", json!({ "command": "rm -rf ./build" }));
    // The human redirects: gate clears, the call gets an error result, the steer
    // follows as a user turn (the real loop's redirect path).
    play.event(crate::tui::TuiEvent::Gate(None));
    play.tool_result(
        "toolu_1",
        "Not run — the user replied instead of approving; see their next message.",
        true,
        WireState::Raw,
    );
    play.user("use `cargo clean` instead");
    play.snapshot("transcript_redirected_gate");
}

/// **Thinking stays in the transcript after the turn commits.** Reasoning streams past far
/// too fast to read at generation speed, so the live phase is only a "something is happening"
/// signal — this is the copy a human actually reads, afterwards, at their own pace. A long
/// block collapses (opening paragraphs plus a `(+N)` hint, so the gist is never hidden) and
/// expands in place; the expand state is keyed by the entry's `seq`, since thinking has no
/// `call_id`.
#[test]
fn committed_thinking_stays_readable_in_the_transcript() {
    use crate::log::{ContentBlock, Payload};

    let mut play = Screenplay::new(80, 24);
    play.user("Why is the cache cold?");
    let reasoning = (1..=12)
        .map(|n| format!("Step {n} of the reasoning."))
        .collect::<Vec<_>>()
        .join("\n\n");
    play.commit(Payload::Assistant {
        thinking: vec![ContentBlock::Thinking {
            text: reasoning,
            signature: "sig".into(),
        }],
        text: vec![ContentBlock::Text("The preamble was re-rendered.".into())],
        calls: vec![],
    });

    // Collapsed by default — but the opening is visible, not hidden behind a header.
    assert!(play.find("✻ thinking").is_some(), "the record is present");
    assert!(
        play.find("Step 1 of the reasoning.").is_some(),
        "…and its opening reads without any interaction"
    );
    assert!(
        play.find("Step 12 of the reasoning.").is_none(),
        "…while the tail is collapsed away"
    );
    // The answer is still the answer — thinking leads the turn, it doesn't replace it.
    assert!(play.find("The preamble was re-rendered.").is_some());

    play.expand_all().pin_to_bottom();
    assert!(
        play.find("Step 12 of the reasoning.").is_some(),
        "expanding reveals the whole record (it runs past one screen — hence scrolling)"
    );
}

/// Encrypted thinking is *named*, not silently dropped — otherwise the transcript would imply
/// the model didn't think when the provider merely withheld the text.
#[test]
fn redacted_thinking_says_so_rather_than_rendering_nothing() {
    use crate::log::{ContentBlock, Payload};

    let mut play = Screenplay::new(80, 12);
    play.user("Go.");
    play.commit(Payload::Assistant {
        thinking: vec![ContentBlock::RedactedThinking {
            data: "encrypted-blob".into(),
        }],
        text: vec![ContentBlock::Text("Done.".into())],
        calls: vec![],
    });
    assert!(play.find("✻ thinking").is_some());
    assert!(
        play.find("encrypted").is_some(),
        "the omission is explained"
    );
    assert!(
        play.find("encrypted-blob").is_none(),
        "…without dumping the opaque payload"
    );
}

/// Renders the `efference gallery` transcript (every element type) headlessly,
/// at a tall terminal so most of it fits — a regression guard for the live
/// design page and a single place to eyeball the whole element set at once.
#[test]
fn gallery() {
    use crate::tui::TuiEvent;
    let mut play = Screenplay::new(80, 90);
    for entry in crate::tui::gallery::log() {
        play.event(TuiEvent::Entry(entry));
    }
    play.snapshot("gallery");
}
