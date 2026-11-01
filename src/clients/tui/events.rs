use super::state::DocumentState;
use crate::clients::tui::document::TuiAction;
use crate::clients::tui::projection::ViewState;

/// Apply a [`TuiAction`] to the document.
///
/// **Nothing here mutates the document.** Folding is render state (docs/tui.md), so
/// `FocusEntry` carries focus and nothing else; `SelectTheme` is handled by the theme
/// picker, and `OpenUrl` is a no-op in the read-only viewer.
///
/// This is kept as the single dispatch point so a per-entry verb that *does* act on
/// the document (fold, open-in-editor) has an obvious home when one is earned.
// Each arm names where its action is actually applied — the caller's cursor, the theme
// picker, `InteractiveState::expanded` — or that it is refused, as an external link is.
// The bodies coincide only in leaving the document alone.
#[allow(clippy::match_same_arms)]
pub(super) fn handle_action(
    _document: &mut DocumentState<'_>,
    _view: &mut ViewState,
    action: &TuiAction<'_>,
) {
    match action {
        // Focus is applied by the caller (the cursor moves, or a click sets it);
        // there is no document change to make.
        TuiAction::FocusEntry(_) => {}
        TuiAction::OpenUrl(_) => { /* external links are not opened */ }
        TuiAction::SelectTheme(_) => { /* handled in the theme picker */ }
        // Expansion is render state the caller owns (`InteractiveState::expanded`).
        TuiAction::ExpandItem(_) => {}
    }
}
