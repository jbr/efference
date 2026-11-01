use crate::clients::tui::harness::Screenplay;
use crossterm::event::{KeyCode, KeyModifiers};

const NONE: KeyModifiers = KeyModifiers::NONE;

/// The menu is attached to the focused unit: a focused tool *result* shows its one
/// verb on its header line, and an unfocused one shows none.
#[test]
fn menu_rides_the_focused_result() {
    let mut play = Screenplay::new(80, 16);
    play.user("look");
    play.tool_exchange("toolu_1", "bash", "ls", "one\ntwo\n");
    assert!(
        play.find("▸ raw").is_none(),
        "no menu until a unit is focused:\n{}",
        play.render()
    );

    play.route_key(KeyCode::Up, NONE); // composer → strip
    play.route_key(KeyCode::Up, NONE); // strip → transcript (last unit: the result)
    let labels: Vec<_> = play
        .state
        .block_menu_items()
        .iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(labels, vec!["raw"], "the result's one verb");
    assert!(
        play.find("▸ raw").is_some(),
        "the menu renders on the focused unit:\n{}",
        play.render()
    );
}

/// Length earns no verb. A long result carries exactly what a short one does,
/// because there is no truncation for a verb to undo (docs/tui.md).
#[test]
fn a_long_result_carries_no_expand_verb() {
    let mut play = Screenplay::new(80, 44);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    let labels: Vec<_> = play
        .state
        .block_menu_items()
        .iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(labels, vec!["raw"], "no expand verb at any length");
}

/// `⏎` on a focused result fires its leading (only) verb — the raw flip, the new
/// home for the purged `r` binding.
#[test]
fn enter_flips_to_raw() {
    let mut play = Screenplay::new(80, 44);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Enter, NONE);
    assert!(play.state.view.is_raw("toolu_1"), "the raw flip fired");
    let labels: Vec<_> = play
        .state
        .block_menu_items()
        .iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(labels, vec!["triaged"], "the flip back is offered");
}

/// The reserved marker cell means moving the selection never shifts the item
/// text — the same rule the strip follows, checked here in the grid. With one
/// verb the move also has to clamp rather than run off the end.
#[test]
fn selection_movement_does_not_shift_text() {
    let mut play = Screenplay::new(80, 44);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    let before = play.find("raw").expect("the raw item renders");
    play.route_key(KeyCode::Right, NONE);
    assert_eq!(play.state.block_menu_focus, 0, "→ clamps at the last verb");
    assert_eq!(
        play.find("raw"),
        Some(before),
        "only the marker cell changes:\n{}",
        play.render()
    );
}

/// The call and the result are separate focus units, and the menu is a function
/// of which one is focused — the call has no per-entry mechanism behind it, so
/// `↑` off the result reaches a unit with no verbs at all.
#[test]
fn the_call_and_its_result_are_separate_units() {
    let mut play = Screenplay::new(80, 24);
    play.user("look");
    play.tool_exchange("toolu_1", "bash", "ls", "one\ntwo\n");
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    assert_eq!(
        play.state
            .block_menu_items()
            .iter()
            .map(|i| i.label)
            .collect::<Vec<_>>(),
        vec!["raw"],
        "the result carries the raw flip"
    );

    play.route_key(KeyCode::Up, NONE); // to the call above it
    assert!(
        play.state.block_menu_items().is_empty(),
        "the call is its own unit, and carries no verbs"
    );
    assert_eq!(
        play.state.block_menu_focus, 0,
        "a new unit starts on its leading verb"
    );
}

/// The gated call's verbs are the gate's, and they live on the strip: there is no
/// result entry at all until it resolves, so nothing carries a raw flip.
#[test]
fn a_gated_call_carries_no_raw_flip() {
    use serde_json::json;
    let mut play = Screenplay::new(80, 16);
    play.user("clean up");
    play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    assert!(
        play.state
            .block_menu_items()
            .iter()
            .all(|i| i.label != "raw"),
        "the pending call has no result to flip"
    );
}

/// A prose unit has no entry verbs, and `⏎` there stays the no-op it was.
#[test]
fn prose_units_carry_no_menu() {
    let mut play = Screenplay::new(80, 14);
    play.user("hello");
    play.assistant("sure — here's an answer", vec![]);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    assert!(
        play.state.block_menu_items().is_empty(),
        "prose carries no verbs"
    );
    play.route_key(KeyCode::Enter, NONE); // must not panic or navigate
}

/// The rendered menu records click targets, and a click fires that item.
#[test]
fn clicking_a_menu_item_fires_it() {
    let mut play = Screenplay::new(80, 44);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    let (x, y) = play.find("▸ raw").expect("the menu renders");
    play.click(x + 2, y);
    assert!(
        play.state.view.is_raw("toolu_1"),
        "the click fired the item under it"
    );
}
