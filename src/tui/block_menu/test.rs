
use crate::tui::harness::Screenplay;
use crossterm::event::{KeyCode, KeyModifiers};

const NONE: KeyModifiers = KeyModifiers::NONE;

/// The menu is attached to the focused unit: a focused tool exchange shows its
/// verbs on the header line, and an unfocused one shows none.
#[test]
fn menu_rides_the_focused_exchange() {
    let mut play = Screenplay::new(80, 16);
    play.user("look");
    play.tool_exchange("toolu_1", "bash", "ls", "one\ntwo\n");
    assert!(
        play.find("▸ raw").is_none(),
        "no menu until a unit is focused:\n{}",
        play.render()
    );

    play.route_key(KeyCode::Up, NONE); // composer → strip
    play.route_key(KeyCode::Up, NONE); // strip → transcript (last unit)
    let labels: Vec<_> = play
        .state
        .block_menu_items()
        .iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(labels, vec!["raw"], "a short result has nothing to expand");
    assert!(
        play.find("▸ raw").is_some(),
        "the menu renders on the focused unit:\n{}",
        play.render()
    );
}

/// A long result is collapsible, so the menu leads with the collapse toggle —
/// keeping `⏎` on a focused unit meaning "expand this" — and the label flips
/// to `collapse` once expanded.
#[test]
fn expand_leads_and_the_label_flips() {
    let mut play = Screenplay::new(80, 20);
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
    assert_eq!(labels, vec!["expand", "raw"], "expand ranks first");

    play.route_key(KeyCode::Enter, NONE); // fire the selected verb
    assert!(
        play.state.view.is_expanded("toolu_1"),
        "⏎ on the menu's first item expands, as it did before the menu"
    );
    let labels: Vec<_> = play
        .state
        .block_menu_items()
        .iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(labels[0], "collapse", "the label names the destination");
}

/// `→ ⏎` reaches the raw flip without the `r` key — the menu is the new home
/// for the binding the purge unit removes.
#[test]
fn right_then_enter_flips_to_raw() {
    let mut play = Screenplay::new(80, 20);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Right, NONE); // expand → raw
    play.route_key(KeyCode::Enter, NONE);
    assert!(play.state.view.is_raw("toolu_1"), "the raw flip fired");
    assert!(
        !play.state.view.is_expanded("toolu_1"),
        "and only that verb fired"
    );
    let labels: Vec<_> = play
        .state
        .block_menu_items()
        .iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(
        labels,
        vec!["expand", "distilled"],
        "the flip back is offered"
    );
}

/// The reserved marker cell means moving the selection never shifts the item
/// text — the same rule the strip follows, checked here in the grid.
#[test]
fn selection_movement_does_not_shift_text() {
    let mut play = Screenplay::new(80, 20);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    let before = play.find("raw").expect("the raw item renders");
    play.route_key(KeyCode::Right, NONE);
    assert_eq!(
        play.find("raw"),
        Some(before),
        "only the marker cell changes:\n{}",
        play.render()
    );
}

/// Selection clamps at both ends and resets when focus moves to another unit,
/// so `⏎` after a `↑`/`↓` always means the leading verb again.
#[test]
fn selection_clamps_and_resets_with_focus() {
    let mut play = Screenplay::new(80, 24);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.tool_exchange("toolu_2", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);

    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Right, NONE);
    assert_eq!(play.state.block_menu_focus, 1, "→ clamps at the last verb");
    play.route_key(KeyCode::Left, NONE);
    play.route_key(KeyCode::Left, NONE);
    assert_eq!(play.state.block_menu_focus, 0, "← clamps at the first");

    play.route_key(KeyCode::Right, NONE);
    play.route_key(KeyCode::Up, NONE); // to the previous unit
    assert_eq!(
        play.state.block_menu_focus, 0,
        "a new unit starts on its leading verb"
    );
}

/// The gated exchange's verbs are the gate's, and they live on the strip: no
/// raw flip on a call that has no result yet.
#[test]
fn the_gated_exchange_carries_no_raw_flip() {
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

/// A prose unit has no entry verbs yet, and `⏎` there stays the no-op it was.
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
    let mut play = Screenplay::new(80, 20);
    play.user("look");
    let long: String = (0..20).map(|i| format!("line {i}\n")).collect();
    play.tool_exchange("toolu_1", "bash", "cat big", &long);
    play.route_key(KeyCode::Up, NONE);
    play.route_key(KeyCode::Up, NONE);
    let (x, y) = play.find("▸ expand").expect("the menu renders");
    play.click(x + 2, y);
    assert!(
        play.state.view.is_expanded("toolu_1"),
        "the click fired the item under it"
    );
}
