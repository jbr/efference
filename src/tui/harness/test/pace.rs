//! The pace widget and its status-bar readout (docs/control-channel.md).

use super::*;

/// Pace (docs/tui.md) is *loud to the operator, silent to the
/// agent*: a held or slowed pace (from the `pace` topic) surfaces a `⏸`/`⏱` indicator in the
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
/// their `+`/`-` mnemonics are gone (the widget owns those keys now).
#[test]
fn pace_widget_steps_and_commits() {
    let none = KeyModifiers::NONE;
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.event(TuiEvent::Phase(crate::daemon::LoopPhase::Requesting));

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
    play.event(TuiEvent::Phase(crate::daemon::LoopPhase::Requesting));
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
    play.event(TuiEvent::Phase(crate::daemon::LoopPhase::Requesting));
    play.route_key(KeyCode::Esc, none); // hold
    play.route_key(KeyCode::Up, none); // → held strip
    play.route_key(KeyCode::Right, none); // interrupt → resume
    play.route_key(KeyCode::Right, none); // → pace
    play.route_key(KeyCode::Enter, none); // enter
    play.snapshot("pace_widget_active");
}
