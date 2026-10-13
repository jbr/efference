//! The composer's selection: mouse placement, extension, cut, paste, and edits
//! that replace it.

use super::*;

impl Composer {
    pub(super) fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// The selected plain text, if the anchor and cursor enclose a range.
    pub(in crate::tui) fn selected_text(&self) -> Option<String> {
        self.selection_range()
            .map(|range| self.text[range].to_string())
    }

    /// Delete the selected text, returning it for a TUI-owned clipboard action.
    pub(in crate::tui) fn cut_selection(&mut self) -> Option<String> {
        let range = self.selection_range()?;
        let text = self.text[range.clone()].to_string();
        self.edit(CoalescingRun::None, |this| {
            this.replace_range(range.clone(), "");
            this.cursor = range.start;
            this.clear_selection();
            this.goal_col = this.cursor_col();
        });
        Some(text)
    }

    /// Insert externally supplied plain text, replacing a selection when present.
    pub(in crate::tui) fn paste_text(&mut self, text: &str) {
        self.edit(CoalescingRun::None, |this| {
            this.insert_text(text);
            this.goal_col = this.cursor_col();
        });
    }

    /// Position the cursor at a byte offset already known to be a grapheme
    /// boundary. A mouse press either clears or starts a selection; a drag keeps
    /// the press position as its anchor.
    pub(in crate::tui) fn set_cursor_from_mouse(&mut self, cursor: usize, extend: bool) {
        debug_assert!(self.text.is_char_boundary(cursor));
        self.break_coalescing();
        if extend {
            self.extend_selection();
        } else {
            self.anchor = Some(cursor);
        }
        self.cursor = cursor;
        if self.anchor == Some(self.cursor) && !extend {
            self.clear_selection();
        }
        self.goal_col = self.cursor_col();
    }

    /// Update the active mouse drag, preserving its press position as the anchor.
    pub(in crate::tui) fn extend_selection_from_mouse(&mut self, cursor: usize) {
        debug_assert!(self.text.is_char_boundary(cursor));
        self.break_coalescing();
        self.extend_selection();
        self.cursor = cursor;
        if self.anchor == Some(self.cursor) {
            self.clear_selection();
        }
        self.goal_col = self.cursor_col();
    }

    /// Map a screen cell in the rendered composer text rectangle to the closest
    /// grapheme boundary. Rows and wrapping are shared with rendering.
    pub(in crate::tui) fn cursor_for_screen_position(
        &self,
        area: Rect,
        column: u16,
        row: u16,
    ) -> usize {
        let rows = self.visual_rows(Self::content_width(area.width));
        if rows.is_empty() {
            return 0;
        }
        let (cursor_vr, _) = self.locate_cursor(&rows);
        let visible = area.height as usize;
        let top = cursor_vr.saturating_sub(visible.saturating_sub(1));
        let screen_row = row.saturating_sub(area.y) as usize;
        let row_index = (top + screen_row).min(top + visible.saturating_sub(1));
        let Some(vr) = rows.get(row_index) else {
            return rows.last().map_or(0, |row| row.end);
        };

        let target = column.saturating_sub(area.x + Self::prompt_cols()) as usize;
        let mut width = 0;
        for (offset, grapheme) in self.text[vr.start..vr.end].grapheme_indices(true) {
            let start = vr.start + offset;
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if target < width + grapheme_width {
                return start;
            }
            width += grapheme_width;
        }
        vr.end
    }

    pub(super) fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub(super) fn extend_selection(&mut self) {
        self.anchor.get_or_insert(self.cursor);
    }

    pub(super) fn replace_selection(&mut self, replacement: &str) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };
        let start = range.start;
        self.replace_range(range, replacement);
        self.cursor = start + replacement.len();
        self.clear_selection();
        true
    }

    pub(super) fn delete_selection(&mut self) -> bool {
        self.replace_selection("")
    }

    pub(super) fn delete_selection_as_edit(&mut self) -> bool {
        if self.selection_range().is_none() {
            return false;
        }
        self.edit(CoalescingRun::None, |this| {
            this.delete_selection();
        });
        true
    }

    pub(super) fn insert_text(&mut self, text: &str) {
        // Sanitize at the single insert point (typed, pasted, staged): the buffer
        // must render. Tabs become the four spaces every render path already
        // displays them as (write_text, render_span widths); other control
        // characters are dropped — they would corrupt the paint, not express
        // intent. `\n` survives: it is a real composer feature (^J).
        let clean: String = text
            .chars()
            .flat_map(|c| match c {
                '\n' => vec![c],
                '\t' => vec![' '; 4],
                c if c.is_control() => vec![],
                c => vec![c],
            })
            .collect();
        if clean.is_empty() {
            return;
        }
        if !self.replace_selection(&clean) {
            let start = self.cursor;
            self.replace_range(start..start, &clean);
            self.cursor = start + clean.len();
        }
    }

    pub(super) fn move_with_selection(
        &mut self,
        extend: bool,
        move_cursor: impl FnOnce(&mut Self),
    ) {
        if extend {
            self.extend_selection();
            move_cursor(self);
            if self.anchor == Some(self.cursor) {
                self.clear_selection();
            }
            return;
        }

        if let Some(range) = self.selection_range() {
            self.cursor = range.start;
            self.clear_selection();
        } else {
            self.clear_selection();
            move_cursor(self);
        }
    }

    pub(super) fn move_right_with_selection(&mut self, extend: bool) {
        if extend {
            self.extend_selection();
            self.move_right();
            if self.anchor == Some(self.cursor) {
                self.clear_selection();
            }
        } else if let Some(range) = self.selection_range() {
            self.cursor = range.end;
            self.clear_selection();
        } else {
            self.move_right();
        }
    }
}
