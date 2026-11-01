//! The composer's visual rows, the cursor's place among them, and the render.

use super::*;

impl Composer {
    /// The number of *visual* rows the buffer occupies once wrapped to the input
    /// area of `area_width` columns — i.e. how tall the input area must be. Shares
    /// the wrap policy with [`render`](Self::render) via [`visual_rows`](Self::visual_rows),
    /// so sizing and painting can never disagree (the bug that panicked: an area
    /// sized to logical lines while render drew more wrapped rows).
    pub(in crate::clients::tui) fn line_count(&self, area_width: u16) -> usize {
        self.visual_rows(Self::content_width(area_width))
            .len()
            .max(1)
    }

    /// Columns available for text after the gutter chrome. Centralized so a future
    /// change to the input decoration (e.g. a bordered box instead of `PROMPT`)
    /// is a localized edit here + the per-row paint, not a change to the wrap math.
    pub(super) fn content_width(area_width: u16) -> usize {
        area_width.saturating_sub(Self::prompt_cols()) as usize
    }

    /// Display width of the left-gutter prompt.
    pub(super) fn prompt_cols() -> u16 {
        UnicodeWidthStr::width(PROMPT) as u16
    }

    /// Wrap every logical line to `content_width` columns, flattened into visual
    /// rows. Each row carries its logical line and the char range `[start, end)`
    /// of that line it covers; the ranges partition the line with no gaps, which
    /// is what lets [`locate_cursor`](Self::locate_cursor) map a caret to a cell.
    /// Always at least one (possibly empty) row per logical line.
    pub(super) fn visual_rows(&self, content_width: usize) -> Vec<VisualRow> {
        let mut rows = Vec::new();
        for line_idx in 0..self.line_starts.len() {
            let range = self.line_range(line_idx);
            let text = &self.text[range.clone()];
            if text.is_empty() || content_width == 0 {
                rows.push(VisualRow {
                    line: line_idx,
                    start: range.start,
                    end: range.start,
                });
                continue;
            }
            let mut start = range.start;
            for byte_break in wrap_points(text, content_width) {
                let end = range.start + byte_break;
                rows.push(VisualRow {
                    line: line_idx,
                    start,
                    end,
                });
                start = end;
            }
            rows.push(VisualRow {
                line: line_idx,
                start,
                end: range.end,
            });
        }
        rows
    }

    /// Find the visual row holding the caret and its byte offset within that row.
    /// A caret resting on a wrap boundary belongs to the *next* row's start,
    /// except on a line's final row where it sits just past the final grapheme.
    pub(super) fn locate_cursor(&self, rows: &[VisualRow]) -> (usize, usize) {
        for (i, vr) in rows.iter().enumerate() {
            if vr.line != self.cursor_row() {
                continue;
            }
            let last_seg = i + 1 >= rows.len() || rows[i + 1].line != vr.line;
            if self.cursor >= vr.start
                && (self.cursor < vr.end || (last_seg && self.cursor == vr.end))
            {
                return (i, self.cursor - vr.start);
            }
        }
        let i = rows.len().saturating_sub(1);
        (i, 0)
    }

    /// Render the composer into `area`, returning the on-screen cursor position so
    /// the caller can place the terminal cursor when the composer is focused.
    /// Long buffers scroll so the cursor row stays visible.
    pub(in crate::clients::tui) fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        text_style: Style,
        prompt_style: Style,
        selection_style: Style,
    ) -> Position {
        if area.width == 0 || area.height == 0 {
            return Position::new(area.x, area.y);
        }
        let prompt_w = Self::prompt_cols();
        let rows = self.visual_rows(Self::content_width(area.width));
        let (cursor_vr, cursor_off) = self.locate_cursor(&rows);

        // Vertical scroll within the input area so the cursor's visual row is shown.
        let visible = area.height as usize;
        let top = cursor_vr.saturating_sub(visible.saturating_sub(1));

        for (screen_row, vr) in rows.iter().enumerate().skip(top).take(visible) {
            let y = area.y + (screen_row - top) as u16;
            let mut x = area.x;
            // Prompt marker on every wrapped row's left gutter.
            for ch in PROMPT.chars() {
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch).set_style(prompt_style);
                }
                x += 1;
            }
            let selection = self.selection_range();
            for (offset, grapheme) in self.text[vr.start..vr.end].grapheme_indices(true) {
                let start = vr.start + offset;
                let end = start + grapheme.len();
                let w = UnicodeWidthStr::width(grapheme) as u16;
                if x + w > area.right() {
                    break;
                }
                let style = if selection
                    .as_ref()
                    .is_some_and(|range| range.start < end && start < range.end)
                {
                    selection_style
                } else {
                    text_style
                };
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(grapheme).set_style(style);
                }
                x += w.max(1);
            }
        }

        // Cursor screen position: prompt + display width up to the caret within
        // its visual row.
        let cursor_screen_row = cursor_vr.saturating_sub(top) as u16;
        let vr = rows[cursor_vr];
        let mut cursor_x = area.x + prompt_w;
        cursor_x += UnicodeWidthStr::width(&self.text[vr.start..vr.start + cursor_off]) as u16;
        Position::new(
            cursor_x.min(area.right().saturating_sub(1)),
            area.y + cursor_screen_row,
        )
    }
}
