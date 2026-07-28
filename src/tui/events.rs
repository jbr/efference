use super::state::DocumentState;
use super::utils::find_node_at_path_mut;
use crate::tui::document::{DocumentNode, TruncationLevel, TuiAction};
use crate::tui::projection::ViewState;

/// Apply a [`TuiAction`] to the document.
///
/// `ExpandBlock` expands a collapsed block in place (the only action that mutates
/// the document). The rustdoc navigation actions are gone; `SelectTheme` is
/// handled by the theme picker, and `OpenUrl` is a no-op in the read-only viewer.
///
/// The in-place flip gives immediate feedback without a reproject (so focus is
/// preserved); for a *keyed* block it also flips the persistent `view` so the
/// state survives the next streamed-token re-projection (docs/tui.md) — the two
/// start in sync (every projection sets the level from `view`), so flipping both
/// keeps them in sync.
///
/// Expanding changes that node's height, so its cached layout is dropped here —
/// the one place the in-place mutation happens, so the one place that has to say so.
pub(super) fn handle_action(
    document: &mut DocumentState<'_>,
    view: &mut ViewState,
    action: TuiAction<'_>,
) {
    match action {
        TuiAction::ExpandBlock(path) => {
            if let Some(DocumentNode::TruncatedBlock { level, key, .. }) =
                find_node_at_path_mut(&mut document.document.nodes, path.indices())
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
                if let Some(&top) = path.indices().first() {
                    document.invalidate_node(top as usize);
                }
            }
        }
        TuiAction::OpenUrl(_) => { /* M1: external links are not opened */ }
        TuiAction::SelectTheme(_) => { /* handled in the theme picker */ }
    }
}
