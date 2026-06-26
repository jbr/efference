use super::utils::find_node_at_path_mut;
use crate::tui::document::{Document, DocumentNode, TruncationLevel, TuiAction};

/// Apply a [`TuiAction`] to the document.
///
/// `ExpandBlock` expands a collapsed block in place (the only action that mutates
/// the document). The rustdoc navigation actions are gone; `SelectTheme` is
/// handled by the theme picker, and `OpenUrl` is a no-op in the read-only viewer.
pub(super) fn handle_action(document: &mut Document<'_>, action: TuiAction<'_>) {
    match action {
        TuiAction::ExpandBlock(path) => {
            if let Some(node) = find_node_at_path_mut(&mut document.nodes, path.indices())
                && let DocumentNode::TruncatedBlock { level, .. } = node
            {
                // Toggle, not a ratchet: expand a collapsed block, and re-collapse an
                // expanded one — so you can peek into a long result and close it back up.
                *level = match level {
                    TruncationLevel::SingleLine | TruncationLevel::Brief => TruncationLevel::Full,
                    TruncationLevel::Full => TruncationLevel::Brief,
                };
            }
        }
        TuiAction::OpenUrl(_) => { /* M1: external links are not opened */ }
        TuiAction::SelectTheme(_) => { /* handled in the theme picker */ }
    }
}
