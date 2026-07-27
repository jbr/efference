use super::{Screenplay, tool_call};
use crate::log::{Keep, WireState};
use crate::tui::{PaceView, TuiEvent};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Color, Modifier};
use serde_json::json;

/// A realistic `edit` result (real format from `.efference/*.jsonl`):
/// preamble lines, then a `===DIFF===` section with ` `/`+`/`-` line prefixes.
const EDIT_RESULT: &str = "Applied replace — the file has been updated.\n\
        Note: the editor applies a consistent formatting style to the entire file, including your edit\n\n\
        ===DIFF===\n\n\
        \x20    /// True when the buffer holds no non-whitespace text.\n\
        \x20    pub(super) fn is_empty(&self) -> bool {\n\
        -        self.lines.iter().all(|l| l.iter().all(|c| c.is_whitespace()))\n\
        +        self.lines\n\
        +            .iter()\n\
        +            .all(|l| l.iter().all(|c| c.is_whitespace()))\n\
        \x20    }";

/// Builds a transcript whose last entry is an `edit` result carrying
/// [`EDIT_RESULT`], so both the snapshot and the color test share one setup.
fn edit_scenario(w: u16, h: u16) -> Screenplay<'static> {
    let mut play = Screenplay::new(w, h);
    play.user("Wrap that long line.");
    play.assistant(
        "Here's the reformatted version.",
        vec![tool_call(
            "edit",
            "toolu_08",
            json!({ "anchor": "fn is_empty", "content": "…" }),
        )],
    );
    play.tool_result("toolu_08", EDIT_RESULT, false, WireState::Raw);
    play
}

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

/// The live background-process panel (docs/supervisor.md): a `Processes` event renders a
/// persistent strip above the composer — one row per running process with handle,
/// command, running/idle timing, and the last output line. Fed by the `/state`
/// poll, it is chrome, not transcript (no reproject).
#[test]
fn background_process_panel() {
    use super::super::{BgProc, TuiEvent};
    let mut play = Screenplay::new(80, 16);
    play.user("watch the build");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.event(TuiEvent::Processes(vec![
        BgProc {
            handle: "bg#1".into(),
            command: "cargo watch -x test".into(),
            tail: "Compiling efference\nrunning 12 tests\ntest result: ok. 12 passed".into(),
            running_secs: 252,
            idle_secs: 8,
        },
        BgProc {
            handle: "bg#2".into(),
            command: "npm run dev".into(),
            tail: "ready - started server on 0.0.0.0:3000".into(),
            running_secs: 9,
            idle_secs: 9,
        },
    ]));
    play.snapshot("background_process_panel");
}

/// A running tool streams raw process bytes into the live tail
/// ([`projection`](super::super::projection) renders `live.tools` directly).
/// That output carries control characters — ANSI color escapes (`\x1b[…m`),
/// carriage returns from progress bars — which a durable, JSON-encoded log
/// never holds, so this path is the only one that can deliver them to a cell.
/// Ratatui's `cell_width` debug-asserts on a control char and panics; the live
/// tail is replayed verbatim on every reattach, so the crash is deterministic.
/// Rendering must instead be total: control chars fold to a glyph, no panic.
#[test]
fn live_tool_output_with_control_chars_does_not_panic() {
    use super::super::TuiEvent;
    let mut play = Screenplay::new(80, 16);
    play.user("run the build");
    play.event(TuiEvent::ToolOutput {
        call_id: "toolu_live".into(),
        chunk: "compiling \x1b[32mok\x1b[0m\rprogress: 50%\rprogress: 100%\n".into(),
    });
    // The assertion is simply that drawing the frame returns rather than panics.
    let screen = play.render();
    assert!(screen.contains("progress"), "live tool output is on screen");
    assert!(
        !screen.contains('\x1b') && !screen.contains('\r'),
        "no raw control chars survive into the rendered grid",
    );
}

/// A token with no break point that is wider than the whole text column (a
/// long URL or path in a narrow terminal). The inline-wrap loop must hard-cut
/// it to make progress; without that it wraps in place forever, incrementing
/// `pos.y` until the `u16` overflows and the render panics.
#[test]
fn long_unbreakable_token_does_not_livelock() {
    let mut play = Screenplay::new(40, 20);
    let tok = "x".repeat(200);
    play.assistant(&format!("here: {tok} done"), vec![]);
    let screen = play.render();
    // It renders (no panic) and the token's tail survives the hard cuts.
    assert!(screen.contains("xxxx"), "the long token is on screen");
    assert!(
        screen.contains("done"),
        "text after the token still renders"
    );
}

/// Unpack an (asserted) RGB cell color into channels.
fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("expected an RGB diff color, got {other:?}"),
    }
}

/// Renders the `efference gallery` transcript (every element type) headlessly,
/// at a tall terminal so most of it fits — a regression guard for the live
/// design page and a single place to eyeball the whole element set at once.
#[test]
fn gallery() {
    use super::super::TuiEvent;
    let mut play = Screenplay::new(80, 90);
    for entry in crate::tui::gallery::log() {
        play.event(TuiEvent::Entry(entry));
    }
    play.snapshot("gallery");
}

/// Load the checked-in replay fixture (`fixtures/replay_session.jsonl`, lifted
/// verbatim from a real `.efference/` session) through the same `persist::load`
/// a live attach uses. Every state a replay of it produces is reachable *by
/// construction* — it came off a real daemon, not a test author's hand.
fn replay_fixture() -> Vec<crate::log::LogEntry> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tui/fixtures/replay_session.jsonl");
    crate::persist::load(&path).expect("the replay fixture loads")
}

/// Filmstrip frame 1: the first user turn, the assistant's `bash ls`, its
/// result, and the assistant's prose answer (entries 0–4 of the real session).
#[test]
fn replay_real_session_first_turn() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 30);
    play.replay(&entries[..=4]);
    play.snapshot("replay_real_session_first_turn");
}

/// Filmstrip frame 2: the tail of the whole real session, stream-followed to the
/// bottom. The session carried a spotter `gate` verdict and `ConfigEdit`s (both
/// audit-only — they project to nothing on the transcript) plus an `Ambient`
/// notification (which *does* project). This pins how the permission-bearing tail
/// of a real session renders, and that the audit-only payloads stay invisible.
#[test]
fn replay_real_session_tail() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 24);
    play.replay(&entries).pin_to_bottom();
    play.snapshot("replay_real_session_tail");
}

/// The composer's top edge spills upward (docs/tui.md): `↑` climbs the buffer,
/// snaps to `0,0` (the detent), and only the *next* `↑` crosses — onto the
/// control strip first, then into transcript navigation. Esc returns home from
/// any depth, preserving the draft. Driven through the real `handle_key_event`
/// routing.
#[test]
fn up_arrow_spills_from_composer_then_esc_returns_home() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let none = KeyModifiers::NONE;

    let mut play = Screenplay::new(80, 24);
    play.user("List the tui module.");
    play.assistant(
        "Here.",
        vec![tool_call(
            "bash",
            "toolu_nav",
            json!({ "command": "ls src/tui" }),
        )],
    );
    let listing = (1..=20)
        .map(|n| format!("render_module_{n:02}.rs"))
        .collect::<Vec<_>>()
        .join("\n");
    play.tool_result("toolu_nav", &listing, false, WireState::Raw);

    // A two-line draft; cursor ends on line two.
    play.type_str("draft line one\nline two");
    assert!(play.state.composing, "starts in compose mode");

    play.route_key(KeyCode::Up, none); // climb to line one (goal column kept)
    play.route_key(KeyCode::Up, none); // snap to 0,0 — the detent
    assert!(
        play.state.composing,
        "still composing at the 0,0 detent — one more ↑ spills"
    );

    play.route_key(KeyCode::Up, none); // spill across the top edge
    assert!(
        !play.state.composing,
        "↑ at 0,0 spilled focus out of the composer"
    );
    assert!(
        play.state.strip_focus.is_some(),
        "the spill lands on the control strip first"
    );

    play.route_key(KeyCode::Up, none); // strip → transcript
    assert!(
        play.state.strip_focus.is_none() && !play.state.composing,
        "↑ from the strip continues into transcript navigation"
    );

    play.route_key(KeyCode::Esc, none);
    assert!(play.state.composing, "Esc returns home to the composer");
    assert_eq!(
        play.state.composer.text(),
        "draft line one\nline two",
        "the draft survives the round trip into navigation and back"
    );
}

/// A focused unit shows an obvious left-edge `▌` bar down its full height — a
/// *glyph* marker, so it renders in any theme and in this no-color grid (the
/// prior reversed-video highlight showed in neither). The bar sits in column 0,
/// left of any authorship gutter.
#[test]
fn focused_unit_shows_a_left_bar() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut play = Screenplay::new(80, 30);
    play.user("List the tui module.");
    play.assistant(
        "Here.",
        vec![tool_call(
            "bash",
            "toolu_fb",
            json!({ "command": "ls src/tui" }),
        )],
    );
    let listing = (1..=20)
        .map(|n| format!("render_module_{n:02}.rs"))
        .collect::<Vec<_>>()
        .join("\n");
    play.tool_result("toolu_fb", &listing, false, WireState::Raw);

    play.route_key(KeyCode::Up, KeyModifiers::NONE); // spill → the control strip
    play.route_key(KeyCode::Up, KeyModifiers::NONE); // strip → focus the exchange
    let bar = play.find("▌").expect("the focused unit shows a left bar");
    assert_eq!(bar.0, 0, "the focus bar sits in column 0");
    play.snapshot("transcript_focused_unit");
}

/// Focus moves between *short* exchanges too, not just long collapsible ones —
/// the fix for focus getting stuck on the only block long enough to collapse.
/// Three one-line bash exchanges; `↑` walks the focus bar up through them.
#[test]
fn focus_moves_between_short_exchanges() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 30);
    play.user("Run three quick commands.");
    play.assistant(
        "First.",
        vec![tool_call("bash", "t1", json!({ "command": "pwd" }))],
    );
    play.tool_result("t1", "/repo", false, WireState::Raw);
    play.assistant(
        "Next.",
        vec![tool_call("bash", "t2", json!({ "command": "whoami" }))],
    );
    play.tool_result("t2", "jbr", false, WireState::Raw);
    play.assistant(
        "Last.",
        vec![tool_call("bash", "t3", json!({ "command": "date" }))],
    );
    play.tool_result("t3", "today", false, WireState::Raw);

    play.route_key(KeyCode::Up, none); // spill → the control strip
    play.route_key(KeyCode::Up, none); // strip → focus the last exchange
    let low = play.find("▌").expect("a unit is focused on spill");
    play.route_key(KeyCode::Up, none); // → previous (higher) exchange
    let high = play.find("▌").expect("a unit is still focused");
    assert!(
        high.1 < low.1,
        "↑ moved the focus bar to a higher exchange (short exchanges are focusable now): \
             {high:?} should be above {low:?}",
    );
}

/// Prose turns are focus units too (the flat model): `↑` walks the bar up from
/// an exchange, onto the assistant's prose, then onto the user's prose — no block
/// is skipped.
#[test]
fn focus_includes_prose_turns() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 30);
    play.user("What does project() do?");
    play.assistant(
        "It folds the log into the wire.",
        vec![tool_call(
            "bash",
            "p1",
            json!({ "command": "grep -n fold src" }),
        )],
    );
    play.tool_result("p1", "src/project.rs:12: fold", false, WireState::Raw);

    play.route_key(KeyCode::Up, none); // spill → the control strip
    play.route_key(KeyCode::Up, none); // focus the exchange (last unit)
    let at_exchange = play.find("▌").expect("exchange focused");
    play.route_key(KeyCode::Up, none); // focus the assistant prose
    let at_assistant = play.find("▌").expect("assistant prose focused");
    play.route_key(KeyCode::Up, none); // focus the user prose
    let at_user = play.find("▌").expect("user prose focused");

    assert!(
        at_assistant.1 < at_exchange.1 && at_user.1 < at_assistant.1,
        "focus walks up exchange → assistant prose → user prose without skipping: \
             {at_user:?} < {at_assistant:?} < {at_exchange:?}",
    );
}

/// The symmetric counterpart: once focus is in the transcript, `↓` past the
/// bottom-most unit stops at the control strip, and `↓` again falls back into
/// the composer (docs/tui.md) — one continuous vertical axis.
#[test]
fn down_past_the_last_unit_falls_back_to_the_composer() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let none = KeyModifiers::NONE;

    let mut play = Screenplay::new(80, 30);
    play.user("List the tui module.");
    play.assistant(
        "Here.",
        vec![tool_call(
            "bash",
            "toolu_nav2",
            json!({ "command": "ls src/tui" }),
        )],
    );
    let listing = (1..=20)
        .map(|n| format!("render_module_{n:02}.rs"))
        .collect::<Vec<_>>()
        .join("\n");
    play.tool_result("toolu_nav2", &listing, false, WireState::Raw);

    // Spill upward (empty buffer → one ↑ crosses immediately, onto the strip),
    // then continue into the transcript.
    play.route_key(KeyCode::Up, none);
    play.route_key(KeyCode::Up, none);
    assert!(
        !play.state.composing && play.state.strip_focus.is_none(),
        "spilled into the transcript"
    );

    // ↓ off the bottom unit stops at the strip…
    play.route_key(KeyCode::Down, none);
    assert!(
        !play.state.composing && play.state.strip_focus.is_some(),
        "↓ past the last unit stops at the control strip"
    );
    // …and ↓ again returns to the composer.
    play.route_key(KeyCode::Down, none);
    assert!(
        play.state.composing,
        "↓ from the strip fell back to the composer"
    );
}

/// `C-d` on an *empty* composer quits (shell/REPL EOF); with text in the buffer
/// it's delete-forward and does not quit.
#[test]
fn ctrl_d_quits_only_on_empty_composer() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = KeyModifiers::CONTROL;

    let mut empty = Screenplay::new(80, 10);
    assert!(
        empty.key_quits(KeyCode::Char('d'), ctrl),
        "C-d on an empty composer quits"
    );

    let mut typed = Screenplay::new(80, 10);
    typed.type_str("draft");
    assert!(
        !typed.key_quits(KeyCode::Char('d'), ctrl),
        "C-d with text in the buffer deletes forward, it does not quit"
    );
}

/// In the transcript (navigation context), an unbound key is a clean no-op — it
/// does not leak into the composer. You return to the composer deliberately
/// (Esc / `↓` off the bottom / the spill), not by typing.
#[test]
fn unbound_key_is_a_noop_in_navigation() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 20);
    play.user("hi");
    play.assistant(
        "yo",
        vec![tool_call("bash", "z", json!({ "command": "ls" }))],
    );
    play.tool_result("z", "out", false, WireState::Raw);

    play.route_key(KeyCode::Up, none); // spill → the control strip
    play.route_key(KeyCode::Up, none); // strip → navigation
    assert!(!play.state.composing, "spilled into navigation");
    play.route_key(KeyCode::Char('x'), none); // an unbound letter
    assert!(
        !play.state.composing,
        "an unbound letter does not return focus to the composer"
    );
    assert!(
        play.state.composer.text().is_empty(),
        "and nothing was typed into the composer"
    );
}

/// The right-docked sidebar (docs/tui.md): on a wide terminal, PROCESSES and STATS
/// render in the right column (no bottom strip), with a `│` divider from the
/// transcript. A glanceable readout — focus + proposals come later.
#[test]
fn sidebar_shows_processes_and_stats() {
    use super::super::{BgProc, TuiEvent, UsageStats};
    let mut play = Screenplay::new(110, 20);
    play.user("Watch the build.");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.event(TuiEvent::Processes(vec![BgProc {
        handle: "bg#1".into(),
        command: "cargo watch -x test".into(),
        tail: "test result: ok. 12 passed".into(),
        running_secs: 252,
        idle_secs: 8,
    }]));
    play.event(TuiEvent::Usage(UsageStats {
        input_tokens: 12400,
        output_tokens: 3100,
        cache_read_input_tokens: 8900,
        cache_creation_input_tokens: 5200,
        seconds_since_last_request: Some(8),
    }));
    play.snapshot("sidebar_processes_and_stats");
}

/// The sidebar-only screenshot slice (`Alt-S`) frames just the sidebar columns:
/// narrow, and excluding the transcript — so a wide-terminal capture isn't verbose.
#[test]
fn sidebar_slice_excludes_the_transcript() {
    use super::super::{PolicyEntry, PolicyView, TuiEvent};
    let mut play = Screenplay::new(110, 16);
    play.user("A long transcript line that must NOT appear in a sidebar-only slice.");
    play.assistant("ok", vec![]);
    play.event(TuiEvent::Policy(PolicyView {
        scope: Some("this repo".into()),
        scope_proposal: None,
        commands: vec![PolicyEntry {
            label: "cargo".into(),
            proposed: false,
            seq: None,
        }],
        paths: vec![],
        tools: vec![],
        review_level: Some("human_only".into()),
    }));

    let slice = play.sidebar_grid();
    assert!(
        slice.contains("POLICY"),
        "the slice shows the sidebar:\n{slice}"
    );
    assert!(
        slice.contains("human_only"),
        "the sidebar surfaces the review-level dial:\n{slice}"
    );
    assert!(
        !slice.contains("must NOT appear"),
        "the slice excludes the transcript:\n{slice}"
    );
    let widest = slice.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    assert!(
        widest < 60,
        "the slice is narrow (≈sidebar width), not the full 110 cols; got {widest}"
    );
}

/// A spotter-routed gate renders the spotter's reason inline (docs/permissions.md),
/// just above the affordance, and offers `.allow` to add a rule.
#[test]
fn gate_shows_spotter_reason_inline() {
    let mut play = Screenplay::new(80, 14);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![tool_call(
            "bash",
            "toolu_g",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate_reason(
        "toolu_g",
        "bash",
        json!({ "command": "rm -rf ./build" }),
        Some("deletes outside the git tree — not reversible"),
    );
    let screen = play.render();
    assert!(
        screen.contains("spotter:") && screen.contains("deletes outside the git tree"),
        "the gate shows the spotter's reason inline:\n{screen}"
    );
    assert!(
        screen.contains("allow…"),
        "the control strip offers the allow… affordance:\n{screen}"
    );
    assert!(
        !screen.contains(".allow"),
        "the typed .allow form is no longer advertised (the strip is the legend):\n{screen}"
    );
}

/// The POLICY sidebar section: committed grants plus a *proposed* one rendered in
/// its sorted position with a trailing `⊕` (docs/permissions.md) — `git log` among
/// the commands, `/refs` among the paths. Committing would drop the `⊕` in place.
#[test]
fn sidebar_policy_with_a_tentative_proposal() {
    use super::super::{PolicyEntry, PolicyView, TuiEvent};
    let committed = |label: &str| PolicyEntry {
        label: label.into(),
        proposed: false,
        seq: None,
    };
    let proposed = |label: &str, seq: u64| PolicyEntry {
        label: label.into(),
        proposed: true,
        seq: Some(seq),
    };
    let mut play = Screenplay::new(110, 28);
    play.user("Work on the repo.");
    play.assistant("On it.", vec![]);
    play.event(TuiEvent::Policy(PolicyView {
        scope: Some("work only in this repo".into()),
        scope_proposal: None,
        commands: vec![
            committed("cargo"),
            committed("git diff"),
            proposed("git log", 7),
        ],
        paths: vec![committed(".  rw"), proposed("/refs  r", 9)],
        tools: vec![committed("read"), committed("write")],
        review_level: None,
    }));

    let bar = play.find("⊕").expect("the proposed grant shows a ⊕ marker");
    assert!(
        bar.0 > 80,
        "the proposal marker is in the right-docked sidebar"
    );
    play.snapshot("sidebar_policy_with_proposal");
}

/// The sidebar docks only at/above the width threshold; below it the layout falls
/// back to no sidebar (the drawer is a later step). Guards the render-side
/// width-responsiveness the resize handler relies on.
#[test]
fn sidebar_docks_only_above_the_width_threshold() {
    let mut narrow = Screenplay::new(80, 10);
    narrow.user("hi");
    assert!(
        narrow.find("PROCESSES").is_none(),
        "no sidebar below the dock threshold (80 cols)"
    );

    let mut wide = Screenplay::new(110, 10);
    wide.user("hi");
    assert!(
        wide.find("PROCESSES").is_some(),
        "the sidebar docks at/above the threshold (110 cols)"
    );
}

/// The live `Ctrl+S` dev screenshot path must produce byte-identical output to
/// the committed-snapshot `render` path (both go through `frame_grid`). This is
/// what lets a live capture diff directly against a `.snap` — if it ever drifts,
/// a screenshot would silently stop being comparable to the fixtures.
#[test]
fn dev_screenshot_matches_snapshot_render() {
    let mut play = Screenplay::new(80, 24);
    play.user("Show me the project signature.");
    play.assistant(
        "On it.",
        vec![tool_call(
            "bash",
            "toolu_ss",
            json!({ "command": "grep -n 'fn project' src/project.rs" }),
        )],
    );
    play.tool_result(
        "toolu_ss",
        "12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {",
        false,
        WireState::Raw,
    );
    assert_eq!(
        play.screenshot_grid(),
        play.render(),
        "the live screenshot path must equal the committed-snapshot render",
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

/// The seam-drain's operator-facing half (docs/tui.md): a
/// submitted turn echoes **immediately** as a pending unit — dimmed, led by a `⋯`
/// marker — so a mid-trajectory message never vanishes into the void. When its
/// matching `User` entry streams back (the entry *is* on the wire), the echo
/// resolves: the marker drops and it renders as a normal committed turn.
#[test]
fn submitted_turn_echoes_pending_then_resolves_on_commit() {
    let mut play = Screenplay::new(60, 12);
    play.type_str("hello there");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);

    // The optimistic echo is on screen right away, with the pending marker…
    assert!(
        play.find("⋯").is_some(),
        "the pending marker shows immediately on submit"
    );
    let (tx, ty) = play.find("hello there").expect("pending echo text visible");
    let pending_fg = play.cell(tx, ty).fg;

    // …and the composer was cleared (the text isn't sitting in the input anymore,
    // only in the transcript echo — one occurrence on screen).
    // Now the daemon's `/events` delivers the committed User entry.
    play.user("hello there");

    assert!(
        play.find("⋯").is_none(),
        "the pending marker drops once the User entry commits"
    );
    let (cx, cy) = play.find("hello there").expect("committed turn present");
    let committed_fg = play.cell(cx, cy).fg;
    assert_ne!(
        pending_fg, committed_fg,
        "the in-flight echo is dimmed; the committed turn renders in the normal voice"
    );
}

/// Pace (docs/tui.md) is *loud to the operator, silent to the
/// agent*: a held or slowed pace (polled from `/state`) surfaces a `⏸`/`⏱` indicator in the
/// status bar, and the default (fast, not held) adds no chrome. Pace never reaches the
/// transcript — this is status-bar-only.
#[test]
fn pace_state_is_loud_in_the_status_bar() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");

    // A hold with a retained speed: the indicator shouts HELD and shows the retained pace.
    play.event(TuiEvent::Pace(PaceView {
        speed_ms: 10_000,
        held: true,
    }));
    assert!(play.find("HELD").is_some(), "held pace is loud");
    assert!(
        play.find("10s").is_some(),
        "retained speed shown under the hold"
    );

    // Released but still slowed: HELD drops, the ⏱ speed indicator remains.
    play.event(TuiEvent::Pace(PaceView {
        speed_ms: 10_000,
        held: false,
    }));
    assert!(play.find("HELD").is_none(), "resume drops the HELD");
    assert!(play.find("⏱ 10s").is_some(), "slowed-but-running indicator");

    // Default (fast, not held): no pace chrome at all.
    play.event(TuiEvent::Pace(PaceView {
        speed_ms: 0,
        held: false,
    }));
    assert!(
        play.find("HELD").is_none() && play.find("⏱").is_none(),
        "the default fast pace adds no status chrome"
    );
}

/// The pace widget (docs/tui.md), the strip's inline stepper class: from the
/// held set, `⏎` on the `pace` item enters it, `↑` steps the dial slower in
/// place (preview only — no daemon push), and `⏎` commits the previewed value.
/// `p` still resumes (a plain button); the old `slower`/`faster` buttons and
/// their `+`/`-` mnemonics are gone (the widget replaced them).
#[test]
fn pace_widget_steps_and_commits() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.event(TuiEvent::Running(true));

    // At-rest composer + loop active: one Esc holds. Spill up onto the held strip.
    play.route_key(KeyCode::Esc, none);
    assert!(play.find("HELD").is_some(), "esc holds the loop");
    assert_eq!(
        play.took_pace(),
        Some((None, Some(true))),
        "the hold pushed its own pace command (drained before the widget checks)"
    );
    play.route_key(KeyCode::Up, none); // spill → held strip (interrupt · resume · pace · …)

    // Enter the widget on the pace item.
    play.route_key(KeyCode::Right, none); // interrupt → resume
    play.route_key(KeyCode::Right, none); // resume → pace
    play.route_key(KeyCode::Enter, none); // enter the stepper
    assert!(
        play.state.pace_widget.is_some(),
        "⏎ on pace enters the widget"
    );
    assert!(
        play.find("▴ pace").is_some() && play.find("▾").is_some(),
        "the active stepper shows the ▴ ▾ treatment:\n{}",
        play.render()
    );
    assert!(play.took_pace().is_none(), "entering pushes nothing");

    // `↑` previews the next rung (500ms) without touching the daemon.
    play.route_key(KeyCode::Up, none);
    assert!(
        play.find("500ms").is_some() && play.took_pace().is_none(),
        "↑ previews the next rung, no push:\n{}",
        play.render()
    );

    // `⏎` commits the previewed speed, leaving the hold untouched.
    play.route_key(KeyCode::Enter, none);
    assert_eq!(
        play.took_pace(),
        Some((Some(500), None)),
        "⏎ pushes the previewed speed, hold untouched"
    );
    assert!(play.state.pace_widget.is_none(), "commit leaves the widget");
    assert!(
        play.find("HELD").is_some(),
        "still held after committing pace"
    );

    // The resume button is unaffected — `p` still resumes.
    play.route_key(KeyCode::Char('p'), none);
    assert!(
        play.find("HELD").is_none(),
        "the resume mnemonic still works"
    );
}

/// Esc while the stepper is active reverts to the entry speed and leaves it —
/// the previews were never sent, so nothing reaches the daemon. The gentlest
/// rung of graduated Esc, ahead of return/pause/interrupt (docs/tui.md).
#[test]
fn pace_widget_esc_reverts_without_sending() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("x");
    play.event(TuiEvent::Running(true));
    play.route_key(KeyCode::Esc, none); // hold
    let _ = play.took_pace(); // drain the hold's own pace command
    play.route_key(KeyCode::Up, none); // → held strip
    play.route_key(KeyCode::Right, none); // interrupt → resume
    play.route_key(KeyCode::Right, none); // → pace
    play.route_key(KeyCode::Enter, none); // enter (entry speed 0 = fast)
    play.route_key(KeyCode::Up, none); // preview 500ms
    play.route_key(KeyCode::Up, none); // preview 1s
    assert!(play.find("1s").is_some(), "stepped up to 1s in preview");

    play.route_key(KeyCode::Esc, none); // revert
    assert!(play.state.pace_widget.is_none(), "esc leaves the widget");
    assert!(
        play.find("HELD").is_some() && play.find("500ms").is_none() && play.find("1s").is_none(),
        "reverted to the entry speed (fast), still held:\n{}",
        play.render()
    );
    assert!(
        play.took_pace().is_none(),
        "the widget (preview + revert) never sent a pace command"
    );
}

/// The active stepper owns the strip row: `▴ pace <v> ▾` loud, the commit/revert
/// note in the slack, the other held verbs gone (only the `detach` universal
/// remains at the right).
#[test]
fn pace_widget_active_row() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("go");
    play.event(TuiEvent::Running(true));
    play.route_key(KeyCode::Esc, none); // hold
    play.route_key(KeyCode::Up, none); // → held strip
    play.route_key(KeyCode::Right, none); // interrupt → resume
    play.route_key(KeyCode::Right, none); // → pace
    play.route_key(KeyCode::Enter, none); // enter
    play.snapshot("pace_widget_active");
}

/// Graduated Esc (docs/tui.md) is a **state machine**, not a press-count: each Esc's effect
/// follows the current state. From transcript focus it returns to the composer; then, while the
/// loop is active, the very next Esc pauses (no wasted "return to composer" press), and a
/// further Esc interrupts the in-flight tool.
#[test]
fn graduated_esc_is_state_driven_return_pause_interrupt() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    // The loop is working — the daemon's `/state` poll reports `running: true`.
    play.event(TuiEvent::Running(true));

    // From transcript navigation, Esc returns to the composer — it does not pause.
    play.route_key(KeyCode::Up, none); // spill → strip
    play.route_key(KeyCode::Up, none); // strip → navigation
    assert!(!play.state.composing, "spilled into navigation");
    play.route_key(KeyCode::Esc, none);
    assert!(play.state.composing, "esc returns to the composer");
    assert!(
        play.find("HELD").is_none(),
        "…and does not pause on that press"
    );
    assert!(!play.took_interrupt());

    // Now at rest in the composer with the loop active: the very next Esc pauses.
    play.route_key(KeyCode::Esc, none);
    assert!(
        play.find("HELD").is_some(),
        "esc pauses on the first press from the composer (no wasted press)"
    );
    assert!(!play.took_interrupt());

    // Paused: a further Esc interrupts the in-flight tool.
    play.route_key(KeyCode::Esc, none);
    assert!(
        play.took_interrupt(),
        "esc while held interrupts the in-flight tool"
    );
}

/// The foreground→background handoff, now the strip's `background` item and its
/// scoped mnemonic (docs/tui.md, docs/supervisor.md): `b` fires the detach
/// (`POST /background`) exactly where the item is shown, and matches nothing
/// anywhere else. The handoff only exists for a foreground `bash` child, so the
/// item's presence — and therefore the mnemonic's — tracks the in-flight tool,
/// not merely "something is running."
#[test]
fn b_mnemonic_backgrounds_a_foreground_bash_and_noops_elsewhere() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("run the tests");

    // Idle: the tail alone, no `background` item → `b` matches nothing.
    play.route_key(KeyCode::Up, none); // spill → strip
    play.route_key(KeyCode::Char('b'), none);
    assert!(
        !play.took_background(),
        "idle → no background item, nothing to detach"
    );

    // A non-bash tool mid-flight: still no handoff to offer.
    play.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));
    play.route_key(KeyCode::Char('b'), none);
    assert!(
        !play.took_background(),
        "a non-bash tool can't be handed to the supervisor"
    );

    // Foreground bash: the strip shows `background b`, so `b` fires the detach.
    play.tool_result("toolu_r", "…", false, WireState::Raw);
    play.running_bash("toolu_b", "cargo test");
    play.route_key(KeyCode::Char('b'), none);
    assert!(
        play.took_background(),
        "b detaches the running command into the supervisor"
    );
}

/// When nothing is happening, Esc at the composer is **swallowed** (docs/tui.md) — it does not
/// set a meaningless hold. The pause step is gated on the loop actually being active, so an idle
/// Esc is a silent no-op (contrast the active case above).
#[test]
fn esc_at_an_idle_composer_is_swallowed() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("x");
    // Default state: not streaming, `running` never reported true → idle.
    play.route_key(KeyCode::Esc, none);
    assert!(
        play.find("HELD").is_none(),
        "esc at an idle composer does not pause"
    );
    assert!(!play.took_interrupt(), "…and does not interrupt");
    assert!(play.state.composing, "…and stays in the composer");
}

/// **A turn with no prose is still visibly working** (backlog/dogfooding-friction.md). Thinking
/// and a tool call's streaming input used to reach the TUI not at all — the sink carried only
/// `text_delta` — so the whole window between submitting a turn and the first prose token was
/// blank, and blank is indistinguishable from hung. Each phase shows what it honestly can:
/// thinking its *tail* (what the model is working through now), a tool call its name, then the
/// bytes of input received.
#[test]
fn a_prose_less_generation_phase_is_visible() {
    let mut play = Screenplay::new(80, 12);
    play.user("Count the modules.");

    play.event(TuiEvent::ThinkingDelta("I should look at the ".into()));
    play.event(TuiEvent::ThinkingDelta("src directory first".into()));
    assert!(
        play.find("thinking").is_some(),
        "a thinking phase is not silent"
    );
    assert!(
        play.find("I should look at the ").is_some() && play.find("src directory first").is_some(),
        "…and shows the thinking in full, not a truncated preview — the point is to watch the \
             work as it happens"
    );

    // The call is named as soon as it opens, before any argument bytes arrive.
    play.event(TuiEvent::ToolCallStart {
        name: "bash".into(),
    });
    assert!(
        play.find("calling bash").is_some(),
        "the pending call is named before its input streams"
    );
    assert!(
        play.find("src directory first").is_none(),
        "acting supersedes thinking — one phase at a time"
    );

    // The arriving input is shown verbatim — never parsed, but not hidden either: the
    // identifying fields come first in JSON, so the head is the informative part.
    play.event(TuiEvent::ToolCallInput {
        fragment: "{\"comm".into(),
    });
    play.event(TuiEvent::ToolCallInput {
        fragment: "and\":\"ls src\"}".into(),
    });
    assert!(
        play.find(r#"{"command":"ls src"}"#).is_some(),
        "fragments accumulate into the raw input, shown in full as text — never parsed as a \
             complete document, and never truncated"
    );

    // Prose is its own evidence of progress, so it replaces the phase line.
    play.event(TuiEvent::AssistantDelta("Looking now.".into()));
    assert!(
        play.find("calling bash").is_none(),
        "prose supersedes phase"
    );
    assert!(play.find("Looking now.").is_some());
}

/// The phase line is *live*, not transcript: once the assistant entry commits, the committed
/// record is the thing to read and the phase has nothing left to report. (Rendering committed
/// thinking in the transcript is a separate unit — docs/transport.md.)
#[test]
fn a_committed_turn_clears_the_live_phase() {
    let mut play = Screenplay::new(80, 12);
    play.user("Count the modules.");
    play.event(TuiEvent::ThinkingDelta("weighing the options".into()));
    assert!(play.find("thinking").is_some());

    play.assistant("Twelve.", vec![]);
    assert!(
        play.find("thinking").is_none(),
        "the committed turn supersedes the live phase"
    );
    assert!(play.find("Twelve.").is_some());
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
