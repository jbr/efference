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
mod scenarios {
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
}
