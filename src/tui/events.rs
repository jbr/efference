use super::utils::find_node_at_path_mut;
use crate::tui::document::{Document, DocumentNode, TruncationLevel, TuiAction};
use crate::tui::projection::ViewState;

/// Apply a [`TuiAction`] to the document.
///
/// `ExpandBlock` expands a collapsed block in place (the only action that mutates
/// the document). The rustdoc navigation actions are gone; `SelectTheme` is
/// handled by the theme picker, and `OpenUrl` is a no-op in the read-only viewer.
///
/// The in-place flip gives immediate feedback without a reproject (so focus is
/// preserved); for a *keyed* block it also flips the persistent `view` so the
/// state survives the next streamed-token re-projection (DESIGN §10.5) — the two
/// start in sync (every projection sets the level from `view`), so flipping both
/// keeps them in sync.
pub(super) fn handle_action(document: &mut Document<'_>, view: &mut ViewState, action: TuiAction<'_>) {
    match action {
        TuiAction::ExpandBlock(path) => {
            if let Some(DocumentNode::TruncatedBlock { level, key, .. }) =
                find_node_at_path_mut(&mut document.nodes, path.indices())
            {
                // Toggle, not a ratchet: expand a collapsed block, and re-collapse an
                // expanded one — so you can peek into a long result and close it back up.
                *level = match level {
                    TruncationLevel::SingleLine | TruncationLevel::Brief => TruncationLevel::Full,
                    TruncationLevel::Full => TruncationLevel::Brief,
                };
                if let Some(call_id) = key.as_deref() {
                    view.toggle_expanded(call_id);
                }
            }
        }
        TuiAction::OpenUrl(_) => { /* M1: external links are not opened */ }
        TuiAction::SelectTheme(_) => { /* handled in the theme picker */ }
    }
}
