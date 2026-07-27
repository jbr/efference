//! Scroll offset, the scrollbar, and the keyboard link cursor.

use super::*;

impl<'a> InteractiveState<'a> {
    /// Set scroll offset with automatic clamping to valid range
    pub(in crate::tui) fn set_scroll_offset(&mut self, offset: u16) {
        self.viewport.scroll_offset = offset;
        // Clamp to valid range if we have layout info
        if let Some(cache) = self.viewport.cached_layout {
            let max_scroll = cache
                .document_height
                .saturating_sub(self.viewport.last_viewport_height);
            self.viewport.scroll_offset = self.viewport.scroll_offset.min(max_scroll);
        }
    }

    /// The maximum scroll offset given the cached layout (0 if unknown).
    pub(in crate::tui) fn max_scroll(&self) -> u16 {
        self.viewport
            .cached_layout
            .map(|cache| {
                cache
                    .document_height
                    .saturating_sub(self.viewport.last_viewport_height)
            })
            .unwrap_or(0)
    }

    /// Pin the viewport to the bottom (used while streaming, when stuck_to_bottom).
    pub(in crate::tui) fn scroll_to_bottom(&mut self) {
        self.viewport.scroll_offset = self.max_scroll();
    }

    /// Check if position is in the scrollbar column
    pub(in crate::tui) fn is_in_scrollbar(&self, pos: Position, content_area_width: u16) -> bool {
        // Scrollbar is at content_area_width (which is frame.width - 1)
        pos.x == content_area_width && pos.y < self.viewport.last_viewport_height
    }

    /// Check if scrollbar should be visible (document taller than viewport)
    pub(in crate::tui) fn scrollbar_visible(&self) -> bool {
        self.viewport
            .cached_layout
            .map(|cache| cache.document_height > self.viewport.last_viewport_height)
            .unwrap_or(false)
    }

    /// Check if a link (by action index) is visible in the current viewport
    pub(in crate::tui) fn is_link_visible(&self, action_index: usize) -> Option<bool> {
        let (rect, _) = self.render_cache.actions.get(action_index)?;
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        // Link is visible if its rect overlaps with the viewport
        Some(rect.y < viewport_bottom && rect.bottom() > viewport_top)
    }

    /// Determine if a focused link is above or below the viewport
    ///
    /// Returns Some(true) if above, Some(false) if below, None if visible or invalid index.
    pub(in crate::tui) fn is_link_off_screen(&self, action_index: usize) -> Option<bool> {
        let (rect, _) = self.render_cache.actions.get(action_index)?;
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        if rect.bottom() <= viewport_top {
            Some(true) // Above viewport
        } else if rect.y >= viewport_bottom {
            Some(false) // Below viewport
        } else {
            None // Visible
        }
    }

    /// Find the first visible link index
    pub(in crate::tui) fn first_visible_link(&self) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Find the last visible link index
    pub(in crate::tui) fn last_visible_link(&self) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Find the next visible link below the current one
    pub(in crate::tui) fn next_visible_link(&self, current_index: usize) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .skip(current_index + 1)
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Find the previous visible link above the current one
    pub(in crate::tui) fn prev_visible_link(&self, current_index: usize) -> Option<usize> {
        let viewport_top = self.viewport.scroll_offset;
        let viewport_bottom = viewport_top + self.viewport.last_viewport_height;

        self.render_cache
            .actions
            .iter()
            .enumerate()
            .take(current_index)
            .rev()
            .find(|(_, (rect, _))| rect.y >= viewport_top && rect.y < viewport_bottom)
            .map(|(idx, _)| idx)
    }

    /// Reset keyboard cursor (called on navigation to new document)
    pub(in crate::tui) fn reset_keyboard_cursor(&mut self) {
        self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
    }
}
