//! The block menu (docs/tui.md): the *entry verbs* of the focused transcript
//! unit, rendered trailing that unit's header line and travelling with `↑`/`↓`
//! focus. Two verb families, two homes — loop verbs (pause, kill, approve…) live
//! on the [control strip](super::control_strip), per-entry verbs (expand/collapse,
//! raw/distilled) live here, at the entry they act on.
//!
//! The menu trails the header text rather than being right-aligned or given its
//! own row: appending never clobbers the header (a long `bash` command keeps its
//! columns) and never reflows the transcript as focus moves, which a dedicated row
//! would do on every `j`/`k`. It is clipped at the right edge when the header runs
//! long, the same fit-to-width rule the strip uses.
//!
//! Which verbs an entry carries is a function of the entry itself, computed by
//! [`menu_items`] from facts both the renderer (which has the node in hand) and
//! the key/mouse dispatch (which looks the node up) can supply — one list, two
//! callers, no drift. Most entries carry **no verbs at all**: there is nothing to
//! expand (entries render whole — docs/tui.md), and the only per-entry mechanism
//! that exists so far is the tool result's distilled/raw fork. A focused entry with
//! an empty menu is the ordinary case, and `⏎` there does nothing rather than
//! reaching for a verb it doesn't have.

use ratatui::{buffer::Buffer, layout::Rect};

use super::InteractiveState;
use super::document::{DocumentNode, EntryKind, TuiAction};
use super::state::KeyboardCursor;
use super::utils::find_node_at_path;

/// A verb the block menu can carry. Each dispatches onto a mechanism that also
/// exists without the menu (the `r` raw flip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockVerb {
    /// Flip the focused tool result between its distilled and raw output.
    ToggleRaw,
    /// Flip a *running* tool's output between the trailing-lines peek and the whole
    /// output so far (docs/tui.md).
    TogglePeek,
}

/// One menu item: the label shown, and the verb `⏎` (or a click) fires. Labels
/// name the *destination* state (`expand` on a collapsed block, `distilled` on one
/// already showing raw), like the strip's `pause`/`resume`.
#[derive(Debug, Clone, Copy)]
pub(super) struct MenuItem {
    pub label: &'static str,
    pub verb: BlockVerb,
}

/// The verbs one entry carries. Empty for every kind but the two tool-output ones
/// — the only entries with a per-entry mechanism behind them.
///
/// - `kind`: what the entry is. A [`ToolResult`](EntryKind::ToolResult) has the
///   distillate/raw fork (docs/redaction-fork.md); a
///   [`RunningTool`](EntryKind::RunningTool) has the peek/full fork (docs/tui.md).
/// - `key`: the entry's durable identity — the `call_id` the flip is recorded
///   against. An entry with none carries no verbs: the toggle would not survive
///   the next re-projection. A running tool's peek carries one now (the `call_id`
///   arrives on `tool_call_start`, docs/control-api.md); it did not, which is why a
///   live block toggled open re-collapsed on the next streamed fragment.
/// - `flipped`: whether this entry's toggle is currently on, which is what the
///   label names — labels name the *destination* state.
pub(super) fn menu_items(kind: EntryKind, key: Option<&str>, flipped: bool) -> Vec<MenuItem> {
    match (kind, key) {
        (EntryKind::ToolResult, Some(_)) => vec![MenuItem {
            label: if flipped { "distilled" } else { "raw" },
            verb: BlockVerb::ToggleRaw,
        }],
        (EntryKind::RunningTool, Some(_)) => vec![MenuItem {
            label: if flipped { "peek" } else { "full" },
            verb: BlockVerb::TogglePeek,
        }],
        _ => Vec::new(),
    }
}

impl InteractiveState<'_> {
    /// The focused unit's menu items, looked up from the document — the dispatch
    /// side of [`menu_items`], used by the key and mouse handlers. Empty when
    /// nothing is focused, and for every entry kind that carries no verbs (which is
    /// most of them).
    pub(super) fn block_menu_items(&self) -> Vec<MenuItem> {
        let KeyboardCursor::Focused { action_index } = self.viewport.keyboard_cursor else {
            return Vec::new();
        };
        let Some((_, TuiAction::FocusEntry(path))) = self.render_cache.actions.get(action_index)
        else {
            return Vec::new();
        };
        let Some(DocumentNode::Entry { kind, key, .. }) =
            find_node_at_path(&self.document.document.nodes, path.indices())
        else {
            return Vec::new();
        };
        let key = key.as_deref();
        menu_items(*kind, key, self.view.is_flipped(*kind, key))
    }

    /// The selected item's index, clamped to the current menu (the focused unit
    /// may have changed under it).
    fn block_menu_index(&self, len: usize) -> usize {
        self.block_menu_focus.min(len.saturating_sub(1))
    }

    /// Move the menu selection (`←`/`→`, `C-b`/`C-f`), clamped at both ends like
    /// the strip's — no wraparound, so the ends are a felt stop.
    pub(super) fn move_block_menu(&mut self, forward: bool) {
        let len = self.block_menu_items().len();
        if len == 0 {
            return;
        }
        let idx = self.block_menu_index(len);
        self.block_menu_focus = if forward {
            (idx + 1).min(len - 1)
        } else {
            idx.saturating_sub(1)
        };
        self.dirty = true;
    }

    /// Fire the selected verb. An entry with no menu is the ordinary case, and `⏎`
    /// there falls through to the plain activation — which, for a transcript entry,
    /// does nothing: focus is the affordance, and no verb has been earned yet
    /// (docs/tui.md).
    pub(super) fn activate_block_menu(&mut self) {
        let items = self.block_menu_items();
        let Some(item) = items.get(self.block_menu_index(items.len())).copied() else {
            self.handle_activate_focused_link();
            return;
        };
        match item.verb {
            BlockVerb::ToggleRaw => self.toggle_raw_focused(),
            BlockVerb::TogglePeek => self.toggle_peek_focused(),
        }
        self.dirty = true;
    }

    /// Paint the menu at `col` on the focused unit's header `row`, each item as
    /// `▸ label ` with a reserved marker cell (a space when unselected, so moving
    /// the selection never shifts text) and the strip's button styling. Records
    /// each item's document-coordinate rect in
    /// [`block_menu_hits`](InteractiveState::block_menu_hits) for click dispatch.
    pub(super) fn render_block_menu(
        &mut self,
        buf: &mut Buffer,
        items: &[MenuItem],
        row: u16,
        col: u16,
        area: Rect,
    ) {
        let selected = self.block_menu_index(items.len());
        let mut x = col;
        for (i, item) in items.iter().enumerate() {
            let style = if i == selected {
                self.theme.strip_item_focused_style
            } else {
                self.theme.strip_item_style
            };
            let marker = if i == selected { "▸ " } else { "  " };
            let text = format!("{marker}{} ", item.label);
            self.write_text(buf, row, x, &text, area, style);
            let width = text.chars().count() as u16;
            self.block_menu_hits.push((Rect::new(x, row, width, 1), i));
            x += width + 1;
        }
    }
}

#[cfg(test)]
mod test;
