use super::StripVerb;
use crate::tui::TuiEvent;
use crate::tui::harness::Screenplay;
use crossterm::event::{KeyCode, KeyModifiers};

const NONE: KeyModifiers = KeyModifiers::NONE;

/// The item labels in rank order — the strip's contents model, asserted
/// directly. (The status bar's hint text mentions some of the same words, so
/// `find` can't carry a negative claim about the strip.)
fn labels(play: &Screenplay) -> Vec<&'static str> {
    play.state.strip_items().iter().map(|e| e.label).collect()
}

/// The strip is always present and never empty: an idle session has no context
/// verbs at all, so what shows is exactly the omnipresent tail, in tail order,
/// directly above the composer.
#[test]
fn strip_is_always_present_and_never_empty() {
    let mut play = Screenplay::new(80, 10);
    play.user("hello");
    assert_eq!(
        labels(&play),
        vec!["pace", "sessions", "theme", "help", "detach"],
        "idle is the tail alone"
    );
    let (px, _) = play.find("pace").expect("pace always shows");
    let (dx, _) = play.find("detach").expect("detach is in the tail");
    assert!(px < dx, "the tail runs most- to least-protected");
    play.snapshot("strip_idle_set");
}

/// The island owner's leaving verb ends the host with it, so it reads `quit`;
/// it is the same verb in the same place, bound to the same key.
#[test]
fn owning_the_host_labels_the_leaving_verb_quit() {
    let mut play = Screenplay::new(80, 10);
    play.state.owns_host = true;
    play.user("hello");
    assert_eq!(
        labels(&play),
        vec!["pace", "sessions", "theme", "help", "quit"]
    );
    let leave = play.state.strip_items().pop().unwrap();
    assert_eq!(leave.verb, StripVerb::Detach);
    assert_eq!(leave.hint, "q");
}

/// The context verbs swap with the state while the tail stays put — the tail is
/// omnipresent, so a state change never costs the operator `pace`/`theme`/
/// `help`/`detach`.
#[test]
fn strip_context_verbs_swap_over_a_fixed_tail() {
    let mut play = Screenplay::new(80, 12);
    play.user("run it");
    assert!(
        !labels(&play).contains(&"pause"),
        "no loop verbs while idle"
    );

    play.running_bash("toolu_b", "cargo test");
    assert_eq!(
        labels(&play),
        vec![
            "background",
            "kill",
            "pause",
            "pace",
            "sessions",
            "theme",
            "help",
            "detach"
        ],
        "the foreground-bash verbs lead, the tail follows unchanged"
    );
    play.snapshot("strip_running_bash");
}

/// `background` is foreground-bash-only — it adopts a live child into the
/// supervisor, and there is no child to adopt anywhere else. The interrupt is
/// not: it stops the loop and withholds the call's output whatever the tool
/// (docs/control-channel.md), so it stays, reading `interrupt` where it isn't
/// also killing a process group.
#[test]
fn strip_offers_background_only_for_foreground_bash_but_always_an_interrupt() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 12);
    play.user("read it");

    play.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));
    assert_eq!(
        labels(&play),
        vec![
            "interrupt",
            "pause",
            "pace",
            "sessions",
            "theme",
            "help",
            "detach"
        ],
        "a non-bash tool still offers the interrupt, without the handoff"
    );

    // A *backgrounded* bash call returns at once and runs under the supervisor
    // (docs/supervisor.md) — it is never the foreground tool either.
    play.running_tool(
        "toolu_g",
        crate::bash::NAME,
        json!({ "command": "cargo watch", "background": true }),
    );
    assert!(
        !labels(&play).contains(&"background"),
        "an already-backgrounded call offers no handoff"
    );
}

/// The non-bash `interrupt` fires the same `POST /loop/interrupt` the bash `kill`
/// does — one endpoint, two labels for the two promises it keeps. Before the
/// latch this item would have been a button that did nothing: nothing awaits
/// the abort future during an MCP call, so the notify had nowhere to land.
#[test]
fn strip_interrupt_on_a_non_bash_tool_fires_the_interrupt() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 12);
    play.user("read it");
    play.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));

    play.route_key(KeyCode::Up, NONE); // → strip, landing on `interrupt`
    play.route_key(KeyCode::Enter, NONE);
    assert!(
        play.took_interrupt(),
        "⏎ on interrupt posts the interrupt for a tool it cannot kill"
    );
}

/// While the model is generating — running, but nothing called yet — there is
/// no process to kill and no child to hand off, but the interrupt still
/// applies: it latches, and the call the model is about to make never runs
/// (docs/control-channel.md).
#[test]
fn strip_while_generating_offers_the_interrupt_and_the_hold() {
    let mut play = Screenplay::new(80, 12);
    play.user("think about it");
    play.event(TuiEvent::Running(true));
    assert_eq!(
        labels(&play),
        vec![
            "interrupt",
            "pause",
            "pace",
            "sessions",
            "theme",
            "help",
            "detach"
        ]
    );
}

/// `held` is a modifier, not a state: wherever the hold verb appears it reads
/// `resume`, including at idle — a hold set mid-trajectory outlives the turn,
/// so the way out of it stays reachable.
#[test]
fn strip_hold_reads_resume_and_survives_into_idle() {
    let mut play = Screenplay::new(80, 12);
    play.user("go");
    play.running_bash("toolu_b", "cargo test");
    play.event(TuiEvent::Pace(super::super::PaceView {
        speed_ms: 0,
        held: true,
    }));
    assert!(labels(&play).contains(&"resume"), "held renames the verb");
    assert!(!labels(&play).contains(&"pause"));

    play.event(TuiEvent::Running(false));
    assert_eq!(
        labels(&play),
        vec!["resume", "pace", "sessions", "theme", "help", "detach"],
        "resume is still reachable once the turn ends"
    );
}

/// Fit-to-width cuts from the right, so a narrow terminal loses the tail's
/// least-relevant end first and never a context verb.
#[test]
fn strip_cuts_from_the_right_when_narrow() {
    let mut wide = Screenplay::new(160, 12);
    wide.user("go");
    wide.running_bash("toolu_b", "cargo test");
    assert!(
        wide.find("detach").is_some(),
        "a wide terminal fits the whole list"
    );

    let mut narrow = Screenplay::new(40, 12);
    narrow.user("go");
    narrow.running_bash("toolu_b", "cargo test");
    assert!(
        narrow.find("background").is_some(),
        "the most relevant verb survives the cut"
    );
    assert!(
        narrow.find("detach").is_none(),
        "the tail's far end is what's lost"
    );
}

/// `↑` from the composer lands on the strip's leftmost (most relevant) item,
/// marked with `▸` in the reserved marker cell; `→`/`←` move between items and
/// clamp at the ends — and focus movement never shifts the labels' columns.
#[test]
fn strip_focus_marker_moves_without_shifting_text() {
    let mut play = Screenplay::new(80, 12);
    play.user("go");
    play.running_bash("toolu_b", "cargo test");
    let pause_before = play.find("pause").expect("pause on screen");

    play.route_key(KeyCode::Up, NONE);
    assert_eq!(
        play.state.strip_focus,
        Some(StripVerb::Background),
        "↑ lands on the first item"
    );
    let m1 = play.find("▸").expect("the focused item shows the marker");
    assert!(m1.0 < play.find("background").unwrap().0);

    play.route_key(KeyCode::Right, NONE);
    let m2 = play.find("▸").expect("marker follows focus");
    assert!(m2.0 > m1.0, "→ moved the marker right");
    assert_eq!(
        play.find("pause").unwrap(),
        pause_before,
        "focus movement does not shift item text"
    );

    play.route_key(KeyCode::Left, NONE);
    play.route_key(KeyCode::Left, NONE);
    assert_eq!(
        play.state.strip_focus,
        Some(StripVerb::Background),
        "← clamps at the left end"
    );
}

/// `⏎` on the focused item fires its verb through the same mechanism as the
/// keybinding: pause holds the loop (status bar shows HELD) and the strip's
/// set flips to the held family, with focus landing on `resume`.
#[test]
fn strip_enter_fires_the_focused_verb() {
    let mut play = Screenplay::new(80, 12);
    play.user("go");
    play.event(TuiEvent::Running(true));
    play.route_key(KeyCode::Up, NONE); // → strip, on `interrupt` (generating: the interrupt leads)
    play.route_key(KeyCode::Right, NONE); // → `pause`
    play.route_key(KeyCode::Enter, NONE);
    assert!(play.find("HELD").is_some(), "⏎ on pause holds the loop");
    assert!(
        play.find("resume").is_some(),
        "the set flipped to the held family"
    );
    assert_eq!(
        play.state.strip_focus,
        Some(StripVerb::PauseToggle),
        "focus stays on the verb it was on — the hold, now reading resume"
    );
}

/// The focused item's surface color differs from an unfocused item's — the
/// glyph marker carries focus in the grid, the raised surface carries it in
/// pixels; the grid can't show the latter, so `cell` asserts it.
#[test]
fn strip_focused_item_has_a_raised_surface() {
    let mut play = Screenplay::new(80, 10);
    play.user("hello");
    play.route_key(KeyCode::Up, NONE); // → strip, on `pace`
    play.route_key(KeyCode::Right, NONE); // focus `sessions`
    play.route_key(KeyCode::Right, NONE); // focus `theme`
    let (tx, ty) = play.find("theme").expect("theme on screen");
    let (hx, hy) = play.find("help").expect("help on screen");
    let focused = play.cell(tx, ty);
    let unfocused = play.cell(hx, hy);
    assert_ne!(
        focused.bg, unfocused.bg,
        "the focused item's surface is raised: {focused:?} vs {unfocused:?}"
    );
    assert_ne!(
        focused.bg,
        play.cell(0, ty).bg,
        "an item's surface differs from the document background"
    );
}

/// `⏎` on `detach` leaves the TUI; the daemon and session keep running, which
/// is why it is one verb and not a detach/terminate pair. It sits at the tail's
/// far end — least relevant, first cut, and reachable by keyboard.
#[test]
fn strip_detach_quits() {
    let mut play = Screenplay::new(80, 10);
    play.user("hello");
    play.route_key(KeyCode::Up, NONE); // idle: pace · sessions · theme · help · detach
    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Right, NONE);
    assert!(
        play.key_quits(KeyCode::Enter, NONE),
        "⏎ on detach ends the UI loop"
    );
}

/// A pending gate populates the strip with its full verb set — `approve` on
/// the landing slot (`↑ ⏎` approves), `allow…`, `abort` — plus the dim
/// redirect note.
#[test]
fn strip_gate_set_leads_with_approve() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 14);
    play.user("clean up");
    play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
    assert_eq!(
        labels(&play),
        vec![
            "approve", "allow…", "abort", "pace", "sessions", "theme", "help", "detach"
        ],
        "the gate verbs in rank order, then the tail"
    );
    assert!(
        play.find("type below to redirect").is_some(),
        "the strip carries the redirect note"
    );

    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Enter, NONE);
    assert!(
        play.state.pending_gate.is_none(),
        "⏎ on approve resolved the gate"
    );
    assert!(
        matches!(
            play.took_approval(),
            Some((id, crate::frontend::Approval::Approve, _)) if id == "toolu_g"
        ),
        "…with an Approve decision"
    );
}

/// `abort` sends the same decision the typed `.abort` sentinel produces, and
/// leaves any drafted text in place.
#[test]
fn strip_abort_sends_abort_and_keeps_the_draft() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 14);
    play.user("clean up");
    play.type_str("actually wait");
    play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
    play.route_key(KeyCode::Up, NONE); // to the 0,0 detent of the draft
    play.route_key(KeyCode::Up, NONE); // spill → strip: approve
    play.route_key(KeyCode::Right, NONE); // allow…
    play.route_key(KeyCode::Right, NONE); // abort
    play.route_key(KeyCode::Enter, NONE);
    assert!(
        matches!(
            play.took_approval(),
            Some((_, crate::frontend::Approval::Abort, _))
        ),
        "⏎ on abort sends Abort"
    );
    assert_eq!(
        play.state.composer.text(),
        "actually wait",
        "the draft survives an abort from the strip"
    );
}

/// The stager class end to end (docs/tui.md): activating `allow…` parks the
/// draft, pre-fills the derived grant spec (the bash program) in the composer
/// under an `allow` chip, `⏎` commits a ConfigEdit through the `.allow` path,
/// and the parked draft comes back.
#[test]
fn allow_stager_prefills_commits_and_restores_the_draft() {
    use crate::frontend::Approval;
    use crate::log::ConfigChange;
    use serde_json::json;

    let mut play = Screenplay::new(80, 14);
    play.user("show the log");
    play.type_str("a half-typed thought");
    play.pending_gate("toolu_g", "bash", json!({ "command": "git log --oneline" }));

    play.route_key(KeyCode::Up, NONE); // to the 0,0 detent of the draft
    play.route_key(KeyCode::Up, NONE); // spill → strip: approve
    play.route_key(KeyCode::Right, NONE); // allow…
    play.route_key(KeyCode::Enter, NONE); // stage
    assert!(play.state.staging.is_some(), "allow… enters staging");
    assert!(play.state.composing, "staging edits in the composer");
    assert_eq!(
        play.state.composer.text(),
        "git",
        "the derived spec (the bash program) is pre-filled for editing"
    );
    assert!(
        play.find("draft saved · ⏎ commit · ⎋ cancel").is_some(),
        "the strip note explains the staging state"
    );

    play.type_str(" log"); // widen the prefix: `git` → `git log`
    play.route_key(KeyCode::Enter, NONE); // commit
    match play.took_approval() {
        Some((id, Approval::ConfigEdit { change, .. }, _)) => {
            assert_eq!(id, "toolu_g");
            assert_eq!(
                change,
                ConfigChange::Command {
                    command: "git log".into()
                },
                "the edited spec became the command grant"
            );
        }
        other => panic!("expected a ConfigEdit decision, got {other:?}"),
    }
    assert!(play.state.staging.is_none(), "staging ends on commit");
    assert!(play.state.pending_gate.is_none(), "the gate resolved");
    assert_eq!(
        play.state.composer.text(),
        "a half-typed thought",
        "the parked draft is restored after commit"
    );
}

/// Esc during staging cancels: the parked draft returns, the gate is still
/// pending, and no decision was sent.
#[test]
fn allow_stager_esc_cancels_and_restores() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 14);
    play.user("show the log");
    play.type_str("draft in progress");
    play.pending_gate("toolu_g", "bash", json!({ "command": "git log" }));
    play.route_key(KeyCode::Up, NONE); // detent
    play.route_key(KeyCode::Up, NONE); // spill → strip
    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Enter, NONE); // stage
    assert_eq!(play.state.composer.text(), "git");

    play.route_key(KeyCode::Esc, NONE); // cancel — the gentlest Esc rung
    assert!(play.state.staging.is_none(), "esc ends staging");
    assert_eq!(
        play.state.composer.text(),
        "draft in progress",
        "the parked draft is restored on cancel"
    );
    assert!(
        play.state.pending_gate.is_some(),
        "the gate is still pending"
    );
    assert!(play.took_approval().is_none(), "no decision was sent");
}

/// A gate that resolves out from under the stager (another client, a
/// daemon-side abort) cancels the staging rather than letting its commit fall
/// through to a normal turn submit.
#[test]
fn gate_resolving_elsewhere_cancels_staging() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 14);
    play.user("show the log");
    play.type_str("my draft");
    play.pending_gate("toolu_g", "bash", json!({ "command": "git log" }));
    play.route_key(KeyCode::Up, NONE); // detent
    play.route_key(KeyCode::Up, NONE); // spill → strip
    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Enter, NONE); // stage
    assert!(play.state.staging.is_some());

    play.event(TuiEvent::Gate(None)); // resolved elsewhere
    assert!(
        play.state.staging.is_none(),
        "staging cancels when the gate goes away"
    );
    assert_eq!(
        play.state.composer.text(),
        "my draft",
        "the parked draft is restored"
    );
}

/// The staging composer renders the verb's chip at the prompt — the visible
/// "your next ⏎ commits an argument, not a message" signal.
#[test]
fn staging_renders_the_chip_and_prefill() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 14);
    play.user("clean up");
    play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Enter, NONE); // stage
    play.snapshot("gate_allow_staging");
}

/// **Focus is a verb, and the state moving under it does not re-aim it.**
///
/// The list is rebuilt per frame with the context verbs prepended, so an index-held
/// focus names a different verb after a state flip: aiming at `pace` on an idle loop
/// and pressing `⏎` after the loop starts fires `interrupt`. Focus holds the
/// `StripVerb`, so the same sequence fires the verb the operator aimed at.
#[test]
fn strip_focus_holds_the_verb_the_state_change_used_to_swap() {
    let mut play = Screenplay::new(80, 12);
    play.user("go");
    play.route_key(KeyCode::Up, NONE); // idle: pace · theme · help · detach
    assert_eq!(play.state.strip_focus, Some(StripVerb::PaceWidget));

    // The loop starts between the focus landing and the ⏎. Under an index this put
    // `interrupt` at position 0.
    play.event(TuiEvent::Running(true));
    assert_eq!(
        play.state.strip_focus,
        Some(StripVerb::PaceWidget),
        "focus still names the verb it landed on"
    );
    play.route_key(KeyCode::Enter, NONE);
    assert!(
        !play.took_interrupt(),
        "⏎ fired the verb under the cursor, not the one that moved into the slot"
    );
    assert!(
        play.state.pace_widget.is_some(),
        "…which is the pace stepper the operator was reaching for"
    );
}

/// **Engaging the strip pins the list, and the pin is visible.** While focus (or
/// hover) is on the strip nothing re-ranks: a verb that stops applying greys in
/// place rather than being removed, so no position moves under the operator's
/// hands, and the row says it is pinned so a frozen strip is never mistaken for a
/// current one. Leaving re-ranks.
#[test]
fn strip_freezes_while_engaged_and_greys_what_stops_applying() {
    // Wide enough that the sidebar, the whole item list, and the pinned note all
    // fit — the fit-to-width cut is a separate rule (above) and would otherwise
    // take the items this asserts on.
    let mut play = Screenplay::new(150, 14);
    play.user("go");
    play.running_bash("toolu_b", "cargo test");
    play.route_key(KeyCode::Up, NONE); // → strip, on `background`
    assert_eq!(
        labels(&play),
        vec![
            "background",
            "kill",
            "pause",
            "pace",
            "sessions",
            "theme",
            "help",
            "detach"
        ]
    );
    let kill_at = play.find("kill").expect("kill on the row");
    assert!(
        play.find("pinned · leave to re-rank").is_none(),
        "a pinned row that still matches the state has nothing stale to announce"
    );

    // The command finishes. The verbs that only applied while it ran stop applying.
    play.tool_result("toolu_b", "ok", false, crate::log::WireState::Raw);
    play.event(TuiEvent::Running(false));
    assert_eq!(
        labels(&play),
        vec![
            "background",
            "kill",
            "pause",
            "pace",
            "sessions",
            "theme",
            "help",
            "detach"
        ],
        "the pinned order holds — nothing moved under the operator"
    );
    assert_eq!(
        play.find("kill"),
        Some(kill_at),
        "…so every label is still in the same columns"
    );
    assert!(
        play.find("pinned · leave to re-rank").is_some(),
        "the freeze announces itself once the row stops matching the state"
    );

    // Greyed in place: the dead verb reads differently from a live one, and firing
    // it does nothing rather than doing something else.
    let dead = play.cell(kill_at.0, kill_at.1);
    assert!(
        dead.has(ratatui::style::Modifier::DIM),
        "a verb that stopped applying reads as inert: {dead:?}"
    );
    let (lx, ly) = play.find("theme").expect("a still-applicable verb");
    assert!(
        !play.cell(lx, ly).has(ratatui::style::Modifier::DIM),
        "…and a verb that still applies does not"
    );
    play.route_key(KeyCode::Right, NONE); // → kill, still on the row
    play.route_key(KeyCode::Enter, NONE);
    assert!(
        !play.took_interrupt(),
        "a greyed verb dispatches nothing at all"
    );
    play.snapshot("strip_frozen_while_engaged");

    // Leaving releases the pin; the next engagement sees the current ranking.
    play.route_key(KeyCode::Esc, NONE);
    play.route_key(KeyCode::Up, NONE);
    assert_eq!(
        labels(&play),
        vec!["pace", "sessions", "theme", "help", "detach"],
        "the list re-ranked once focus left"
    );
}

/// Hover is the other engagement: a pointer resting on the row pins it exactly as
/// keyboard focus does, and moving off releases it. Mouse-down engages through the
/// same path, since a click sets focus.
#[test]
fn hovering_the_strip_pins_it_and_leaving_releases_it() {
    let mut play = Screenplay::new(150, 14);
    play.user("go");
    play.running_bash("toolu_b", "cargo test");
    let row = play.strip_row();
    play.hover(4, row);
    assert!(
        play.state.viewport.strip_hovered,
        "the pointer is on the row"
    );
    play.render();

    play.tool_result("toolu_b", "ok", false, crate::log::WireState::Raw);
    play.event(TuiEvent::Running(false));
    assert!(
        labels(&play).contains(&"kill"),
        "the row is pinned under the cursor even with no keyboard focus"
    );

    play.hover(4, 1); // back into the transcript
    assert!(!play.state.viewport.strip_hovered);
    play.render();
    assert_eq!(
        labels(&play),
        vec!["pace", "sessions", "theme", "help", "detach"],
        "leaving the row re-ranks it"
    );
}

/// Rendering records the items' column ranges for mouse hit-testing.
#[test]
fn strip_hits_are_recorded_for_clicks() {
    let mut play = Screenplay::new(80, 10);
    play.user("hello");
    play.render();
    assert!(
        play.state.strip_hits.len() >= 3,
        "idle set + universal recorded: {:?}",
        play.state.strip_hits
    );
}
