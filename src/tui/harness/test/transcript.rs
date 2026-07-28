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

/// A pending approval gate, rendered inline at the tail call rather than as a
/// bottom banner (docs/tui.md): a `⏸ awaiting your approval …` affordance sits beside
/// the exchange, where the result will land — the gate reads as "after the call,
/// before the result." Resolving it replaces the affordance with the real (or error)
/// result. The affordance is a sibling of the exchange, so neither it nor its `⏸`
/// can be collapsed away by a long call.
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

/// **Thinking stays in the transcript after the turn commits, in full.** Reasoning streams
/// past far too fast to read at generation speed, so the live phase is only a "something is
/// happening" signal — this is the copy a human actually reads, afterwards, at their own
/// pace, which it can only be if all of it is here (docs/positions.md).
#[test]
fn committed_thinking_stays_readable_in_the_transcript() {
    use crate::log::{ContentBlock, Payload};

    let mut play = Screenplay::new(80, 60);
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

    assert!(play.find("✻ thinking").is_some(), "the record is present");
    assert!(
        play.find("Step 1 of the reasoning.").is_some(),
        "…and it reads without any interaction"
    );
    assert!(
        play.find("Step 12 of the reasoning.").is_some(),
        "…all of it, not an opening and a hint"
    );
    // The answer is still the answer — thinking leads the turn, it doesn't replace it.
    assert!(play.find("The preamble was re-rendered.").is_some());
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
    play.scroll_to_top();
    play.snapshot("gallery");
}

mod results;
