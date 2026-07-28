use super::state::DocumentState;
use crate::tui::document::TuiAction;
use crate::tui::projection::ViewState;

/// Apply a [`TuiAction`] to the document.
///
/// **Nothing here mutates the document any more.** `FocusEntry` used to be
/// `ExpandBlock`, which flipped a collapse level in place; entries now render whole
/// (docs/tui.md), so the action carries focus and nothing else. The rustdoc
/// navigation actions are gone; `SelectTheme` is handled by the theme picker, and
/// `OpenUrl` is a no-op in the read-only viewer.
///
/// This is kept as the single dispatch point so a per-entry verb that *does* act on
/// the document (fold, open-in-editor) has an obvious home when one is earned.
pub(super) fn handle_action(
    _document: &mut DocumentState<'_>,
    _view: &mut ViewState,
    action: TuiAction<'_>,
) {
    match action {
        // Focus is applied by the caller (the cursor moves, or a click sets it);
        // there is no document change to make.
        TuiAction::FocusEntry(_) => {}
        TuiAction::OpenUrl(_) => { /* M1: external links are not opened */ }
        TuiAction::SelectTheme(_) => { /* handled in the theme picker */ }
    }
}
