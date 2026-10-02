//! Pointer affordances (docs/tui.md, "Hover and click"): a call item tints under the
//! pointer, a strip item lifts, and a click anywhere on a call focuses it.

use super::*;

/// A call and its result read as one card: the pointer anywhere over it tints
/// every row of it, and only it.
#[test]
fn a_call_item_tints_under_the_pointer() {
    let mut play = Screenplay::new(80, 24);
    play.user("look");
    play.tool_exchange("toolu_1", "bash", "ls", "a\nb\nc");
    play.assistant("Done.", vec![]);
    let (_, call_row) = play.find("› ls").expect("the call renders");
    let (_, prose_row) = play.find("Done.").expect("the prose renders");
    let before = play.cell(40, call_row).bg;

    // Hover the result's last line: the whole item lights, the call row included.
    let (_, result_row) = play.find("│ c").expect("the result renders");
    play.hover(40, result_row);
    let lit = play.cell(40, call_row).bg;
    assert_ne!(lit, before, "the call row tints with its result hovered");
    assert_eq!(
        play.cell(40, result_row).bg,
        lit,
        "one surface across the item"
    );
    assert_eq!(
        play.cell(40, prose_row).bg,
        before,
        "prose outside it does not"
    );

    // Away from it, the tint goes.
    play.hover(40, prose_row);
    assert_eq!(play.cell(40, call_row).bg, before);
}

/// A click on any row of a call focuses it — not only its first row.
#[test]
fn a_click_anywhere_on_a_call_focuses_it() {
    let mut play = Screenplay::new(80, 24);
    play.user("look");
    play.tool_exchange("toolu_1", "bash", "ls", "a\nb\nc");
    let (_, row) = play.find("│ b").expect("the result renders");
    play.click(40, row);
    assert!(
        matches!(
            play.state.viewport.keyboard_cursor,
            crate::tui::state::KeyboardCursor::Focused { .. }
        ),
        "the click focused a unit"
    );
    assert!(play.find("▌").is_some(), "the focus bar shows");
}

/// A strip item under the pointer lifts off the row; focus keeps its own surface.
#[test]
fn a_strip_item_lifts_under_the_pointer() {
    let mut play = Screenplay::new(80, 12);
    play.user("hi");
    let row = play.strip_row();
    let (x, _) = play.find("theme t").expect("the strip renders");
    let before = play.cell(x, row).bg;
    play.hover(x, row);
    assert_ne!(play.cell(x, row).bg, before, "the hovered item lifts");
    play.hover(x, row.saturating_sub(3));
    assert_eq!(
        play.cell(x, row).bg,
        before,
        "and settles when the pointer leaves"
    );
}
