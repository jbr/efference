use ratatui::{buffer::Buffer, layout::Rect};

use super::{render_document::BASELINE_LEFT_MARGIN, state::InteractiveState};

impl<'a> InteractiveState<'a> {
    /// Render the status bar at the bottom of the screen.
    ///
    /// Simplified from ferritin: the go-to/search input prompts and the
    /// navigation scope hint are gone (the M1 viewer has no input modes), so the
    /// bar just shows the current status message.
    pub(super) fn render_status_bar(&mut self, buf: &mut Buffer, area: Rect) {
        let style = self.theme.status_style;

        // Clear the status line with the static background.
        for x in 0..area.width {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.reset();
                cell.set_style(style);
            }
        }

        // Render the status message from the left margin, truncating by columns
        // (char-safe — the message may contain multi-byte glyphs). A streaming
        // turn gets a marker prefix.
        let text = if self.loading.pending_request {
            format!("◍ streaming…  {}", self.ui.debug_message)
        } else {
            self.ui.debug_message.to_string()
        };
        for (col, ch) in (BASELINE_LEFT_MARGIN..).zip(text.chars()) {
            if col >= area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((col, area.y)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }
}
