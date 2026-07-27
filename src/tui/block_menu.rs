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
//! Which verbs a block carries is a function of the block itself, computed by
//! [`menu_items`] from facts both the renderer (which has the node in hand) and
//! the key/mouse dispatch (which looks the node up) can supply — one list, two
//! callers, no drift. A block with no durable `key` (a live tool's in-flight
//! output) carries none: its toggles would not survive the next re-projection.

use ratatui::{buffer::Buffer, layout::Rect};

use super::InteractiveState;
use super::document::{DocumentNode, TruncationLevel, TuiAction};
use super::projection::is_call_key;
use super::render_node::is_collapsible;
use super::state::KeyboardCursor;
use super::utils::find_node_at_path;

/// A verb the block menu can carry. Each dispatches onto a mechanism that also
/// exists without the menu (the `⏎` expand toggle, the `r` raw flip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockVerb {
    /// Toggle the focused block's collapse level.
    ToggleExpand,
    /// Flip the focused tool result between its distilled and raw output.
    ToggleRaw,
}

/// One menu item: the label shown, and the verb `⏎` (or a click) fires. Labels
/// name the *destination* state (`expand` on a collapsed block, `distilled` on one
/// already showing raw), like the strip's `pause`/`resume`.
#[derive(Debug, Clone, Copy)]
pub(super) struct MenuItem {
    pub label: &'static str,
    pub verb: BlockVerb,
}

/// The verbs for one block, ranked with the collapse toggle first — so `⏎` on a
/// focused unit keeps meaning "expand this," as it did before the menu existed.
///
/// - `key`: the block's durable identity (`None` → no verbs, see the module docs).
/// - `collapsible`: whether the body has more lines than a collapsed block shows;
///   a short result has nothing to expand, so it carries only the raw flip.
/// - `gated`: this exchange is the one awaiting approval — its verbs are the
///   gate's, and they live on the strip; there is also no result yet to flip.
pub(super) fn menu_items(
    key: Option<&str>,
    level: TruncationLevel,
    collapsible: bool,
    showing_raw: bool,
    gated: bool,
) -> Vec<MenuItem> {
    let Some(key) = key else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if collapsible {
        items.push(MenuItem {
            label: if matches!(level, TruncationLevel::Full) {
                "collapse"
            } else {
                "expand"
            },
            verb: BlockVerb::ToggleExpand,
        });
    }
    // Only a tool exchange has a distillate/raw fork (docs/redaction-fork.md);
    // a thinking block is keyed too, but has one form.
    if is_call_key(key) && !gated {
        items.push(MenuItem {
            label: if showing_raw { "distilled" } else { "raw" },
            verb: BlockVerb::ToggleRaw,
        });
    }
    items
}

impl InteractiveState<'_> {
    /// The focused unit's menu items, looked up from the document — the dispatch
    /// side of [`menu_items`], used by the key and mouse handlers. Empty when
    /// nothing is focused, when the focused unit is a prose run (a `Turn`, which
    /// has no per-entry verbs yet), or when the block carries no verbs.
    pub(super) fn block_menu_items(&self) -> Vec<MenuItem> {
        let KeyboardCursor::Focused { action_index } = self.viewport.keyboard_cursor else {
            return Vec::new();
        };
        let Some((_, TuiAction::ExpandBlock(path))) = self.render_cache.actions.get(action_index)
        else {
            return Vec::new();
        };
        let Some(DocumentNode::TruncatedBlock {
            key, level, nodes, ..
        }) = find_node_at_path(&self.document.document.nodes, path.indices())
        else {
            return Vec::new();
        };
        let key = key.as_deref();
        menu_items(
            key,
            *level,
            is_collapsible(nodes),
            key.is_some_and(|k| self.view.is_raw(k)),
            self.gate_is_on(key),
        )
    }

    /// Whether `key` names the exchange currently awaiting approval.
    pub(super) fn gate_is_on(&self, key: Option<&str>) -> bool {
        match (self.pending_gate.as_ref(), key) {
            (Some(gate), Some(key)) => gate.call_id == key,
            _ => false,
        }
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

    /// Fire the selected verb. With no menu on the focused unit, `⏎` keeps its
    /// plain meaning (toggle whatever the focused action expands), so nothing that
    /// worked before the menu stops working.
    pub(super) fn activate_block_menu(&mut self) {
        let items = self.block_menu_items();
        let Some(item) = items.get(self.block_menu_index(items.len())).copied() else {
            self.handle_activate_focused_link();
            return;
        };
        match item.verb {
            BlockVerb::ToggleExpand => self.handle_activate_focused_link(),
            BlockVerb::ToggleRaw => self.toggle_raw_focused(),
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
