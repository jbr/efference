use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::state::InteractiveState;

impl<'a> InteractiveState<'a> {
    /// Write text to buffer at position
    pub(super) fn write_text(
        &self,
        buf: &mut Buffer,
        row: u16,
        col: u16,
        text: &str,
        area: Rect,
        style: Style,
    ) {
        if row < self.viewport.scroll_offset || row >= self.viewport.scroll_offset + area.height {
            return; // Outside visible area
        }

        let screen_row = row - self.viewport.scroll_offset;
        let mut current_col = col;

        for ch in text.chars() {
            if current_col >= area.width {
                break; // Past right edge
            }

            // Handle tabs: replace with spaces to avoid column counting mismatches
            // Tabs display as multiple spaces in terminals but count as 1 character
            if ch == '\t' {
                // Write 4 spaces for each tab (Rust convention)
                for _ in 0..4 {
                    if current_col >= area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((current_col, screen_row)) {
                        cell.set_char(' ');
                        cell.set_style(style);
                    }
                    current_col += 1;
                }
            } else {
                // Control characters (e.g. ANSI escapes `\x1b`, carriage returns from
                // a running tool's live output — which is never persisted, so it can
                // only arrive here through the in-flight tail) must never reach a
                // ratatui cell: `cell_width` debug-asserts on them and panics. Fold
                // them to the replacement glyph so rendering is total. We keep one
                // cell per char (matching `display_width`) so column accounting in
                // the caller's wrap math stays exact.
                let glyph = if ch.is_control() {
                    '\u{fffd}'
                } else {
                    ch
                };
                if let Some(cell) = buf.cell_mut((current_col, screen_row)) {
                    cell.set_char(glyph);
                    cell.set_style(style);
                }
                current_col += 1;
            }
        }
    }
}
