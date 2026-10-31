//! The pointer's two handlers that read the layout the last frame recorded (docs/tui.md):
//! a move, which sets the hover state, and a left press, which acts on whatever is under it.

use crossterm::event::KeyModifiers;
use ratatui::{Terminal, layout::Position, prelude::Backend};

use crate::tui::{InteractiveState, UiMode};

impl<'a> InteractiveState<'a> {
    /// The pointer moved: the hover state of the header, the scrollbar, the control strip, and
    /// the document under it.
    pub(super) fn pointer_moved(
        &mut self,
        column: u16,
        row: u16,
        terminal: &Terminal<impl Backend>,
    ) {
        let Ok(size) = terminal.size() else {
            return;
        };

        // In ThemePicker mode, use absolute screen coordinates (no scroll offset)
        if matches!(self.ui_mode, UiMode::ThemePicker { .. }) {
            self.viewport.cursor_pos = Some(Position::new(column, row));
            return;
        }

        let terminal_height = size.height;
        let terminal_width = size.width;
        // The transcript spans from under the header to above the bottom
        // chrome; positions in it translate to document coordinates through
        // the origin the frame recorded (docs/tui.md).
        let top = self.viewport.doc_origin.y;
        let content_bottom = terminal_height
            .saturating_sub(1 + self.viewport.input_height + self.viewport.above_input_height);
        let content_width = terminal_width.saturating_sub(1); // Exclude scrollbar column
        let in_drawer = self
            .viewport
            .sidebar_area
            .is_some_and(|area| area.contains(Position::new(column, row)));

        self.viewport.header_hovered = (row < top).then(|| self.header_item_at(column)).flatten();

        // Check if hovering over scrollbar
        self.viewport.scrollbar_hovered = !in_drawer
            && row >= top
            && row < content_bottom
            && column == content_width
            && self.scrollbar_visible();

        // The control-strip row sits directly above the composer
        // (docs/tui.md). A pointer resting on it is an *engagement*: it pins
        // the item list, so nothing re-ranks under the cursor between the
        // operator aiming and clicking. Level, not edge — any move off the
        // row clears it, and the pin releases with it.
        self.viewport.strip_hovered =
            row == terminal_height.saturating_sub(2 + self.viewport.input_height);
        self.viewport.strip_hover = if self.viewport.strip_hovered {
            self.strip_hits
                .iter()
                .find(|(x0, x1, _)| column >= *x0 && column < *x1)
                .map(|(_, _, verb)| *verb)
        } else {
            None
        };

        if !in_drawer && row >= top && row < content_bottom {
            // Mouse in main content area
            self.viewport.cursor_pos = Some(Position::new(
                column.saturating_sub(self.viewport.doc_origin.x),
                row - top + self.viewport.scroll_offset,
            ));
        } else {
            // Header, drawer, or bottom chrome: no document hover targets
            self.viewport.cursor_pos = None;
        }
    }

    /// The left button went down: the composer's cursor, the drawer, the header, the
    /// scrollbar, the transcript, a staged attachment's `✕`, or the control strip,
    /// whichever the pointer is over.
    pub(super) fn left_pressed(
        &mut self,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        terminal: &Terminal<impl Backend>,
    ) {
        if let Some(area) = self.viewport.composer_area
            && area.contains(Position::new(column, row))
        {
            let cursor = self.composer.cursor_for_screen_position(area, column, row);
            self.composer
                .set_cursor_from_mouse(cursor, modifiers.contains(KeyModifiers::SHIFT));
            self.viewport.composer_dragging = true;
            self.composing = true;
            self.strip_focus = None;
            return;
        }

        let Ok(size) = terminal.size() else {
            return;
        };

        let terminal_height = size.height;
        let top = self.viewport.doc_origin.y;
        let content_bottom = terminal_height
            .saturating_sub(1 + self.viewport.input_height + self.viewport.above_input_height); // status row + composer
        let content_height = content_bottom.saturating_sub(top);
        let content_width = size.width.saturating_sub(1); // Exclude scrollbar column

        // The control strip row sits directly above the composer
        // (docs/tui.md); a click on an item focuses and activates it.
        let strip_row = terminal_height.saturating_sub(2 + self.viewport.input_height);

        // The drawer is over the transcript, so it takes its clicks first; a
        // click elsewhere in the transcript closes it, as a click outside the
        // web client's drawer does.
        if let Some(area) = self.viewport.sidebar_area {
            if area.contains(Position::new(column, row)) {
                self.click_drawer(Position::new(column, row));
                return;
            }
            if row >= top && row < content_bottom {
                self.leave_sidebar();
                return;
            }
        }

        if row < top {
            if let Some(item) = self.header_item_at(column) {
                self.activate_header_item(item);
            }
        } else if row < content_bottom && column == content_width && self.scrollbar_visible() {
            // Start scrollbar drag
            self.viewport.scrollbar_dragging = true;
            // Calculate scroll position from click Y
            self.handle_scrollbar_drag(row - top, content_height);
        } else if row < content_bottom {
            // Click in main content area
            self.viewport.clicked_position = Some(Position::new(
                column.saturating_sub(self.viewport.doc_origin.x),
                row - top + self.viewport.scroll_offset,
            ));
        } else if let Some(key) = self
            .viewport
            .staged_hits
            .iter()
            .find(|(y, x0, x1, _)| row == *y && column >= *x0 && column < *x1)
            .map(|(_, _, _, key)| *key)
        {
            // A staged attachment's `✕` (docs/tui.md), resolved to the chip's key.
            self.remove_staged(key);
        } else if row == strip_row
            && let Some(verb) = self
                .strip_hits
                .iter()
                .find(|(x0, x1, _)| column >= *x0 && column < *x1)
                .map(|(_, _, verb)| *verb)
        {
            // The click resolves to the *verb* in that column, not to the
            // column: a click has no dwell, so an index would fire whatever
            // had moved into the position since the frame was painted
            // (docs/tui.md).
            self.composing = false;
            self.strip_focus = Some(verb);
            self.activate_strip_verb(verb);
        }
    }
}
