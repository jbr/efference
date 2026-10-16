//! Keyboard navigation: focus movement between units, and the graduated escape/quit paths.

use super::*;

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
        vec![ToolCall::new(
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
        vec![ToolCall::new(
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
        vec![ToolCall::new("bash", "t1", json!({ "command": "pwd" }))],
    );
    play.tool_result("t1", "/repo", false, WireState::Raw);
    play.assistant(
        "Next.",
        vec![ToolCall::new("bash", "t2", json!({ "command": "whoami" }))],
    );
    play.tool_result("t2", "jbr", false, WireState::Raw);
    play.assistant(
        "Last.",
        vec![ToolCall::new("bash", "t3", json!({ "command": "date" }))],
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
        vec![ToolCall::new(
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
        vec![ToolCall::new(
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
        vec![ToolCall::new("bash", "z", json!({ "command": "ls" }))],
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

/// Graduated Esc (docs/tui.md) is a **state machine**, not a press-count: each Esc's effect
/// follows the current state. From transcript focus it returns to the composer; then, while the
/// loop is active, the very next Esc pauses (no wasted "return to composer" press), and a
/// further Esc interrupts the in-flight tool.
#[test]
fn graduated_esc_is_state_driven_return_pause_interrupt() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    // The loop is working — the daemon's `running` topic reports true.
    play.event(TuiEvent::Phase(crate::daemon::LoopPhase::Requesting));

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
/// scoped mnemonic (docs/tui.md, docs/process-monitor.md): `b` fires the detach
/// (`PATCH /processes/:id {background: true}`) exactly where the item is shown, and matches nothing
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

    // A non-bash tool mid-flight: still no handoff to offer. The state changes with
    // the strip unengaged, which is when it re-ranks (docs/tui.md) — engaged, the
    // list is pinned and the mnemonic follows what is shown.
    play.route_key(KeyCode::Esc, none); // leave the strip, releasing the pin
    play.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));
    play.route_key(KeyCode::Up, none);
    play.route_key(KeyCode::Char('b'), none);
    assert!(
        !play.took_background(),
        "a non-bash tool can't be handed to the process monitor"
    );

    // Foreground bash: the strip shows `background b`, so `b` fires the detach.
    play.route_key(KeyCode::Esc, none);
    play.tool_result("toolu_r", "…", false, WireState::Raw);
    play.running_bash("toolu_b", "cargo test");
    play.route_key(KeyCode::Up, none);
    play.route_key(KeyCode::Char('b'), none);
    assert!(
        play.took_background(),
        "b detaches the running command into the process monitor"
    );
}
