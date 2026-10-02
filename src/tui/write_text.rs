use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::state::InteractiveState;

/// The glyph a cell may hold for `ch`. Control characters (a newline in a process
/// command, an escape from live tool output) never reach a ratatui cell: `cell_width`
/// debug-asserts on them and the TUI thread dies. Every direct `set_char` over
/// content text goes through this; tabs are the caller's business (`write_text`
/// expands them, chrome shows the placeholder).
pub(super) fn fold_control(ch: char) -> char {
    if ch.is_control() { '\u{fffd}' } else { ch }
}

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
        // Outside the visible area — or a measure pass, which lays out without
        // drawing (docs/tui.md).
        let (top, bottom) = self.drawable_rows(area.height);
        if self.layout.measuring || row < top || row >= bottom {
            return;
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
                // One cell per char (matching `display_width`) so column accounting
                // in the caller's wrap math stays exact; see `fold_control` for why
                // control characters are folded rather than written.
                let glyph = fold_control(ch);
                if let Some(cell) = buf.cell_mut((current_col, screen_row)) {
                    cell.set_char(glyph);
                    cell.set_style(style);
                }
                current_col += 1;
            }
        }
    }
}
