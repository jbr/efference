use ratatui::{buffer::Buffer, layout::Rect};

use super::{render_document::BASELINE_LEFT_MARGIN, state::InteractiveState};

impl<'a> InteractiveState<'a> {
    /// Render the status bar at the bottom of the screen.
    ///
    /// The left side shows the status message (and streaming indicator if pending).
    /// The right side shows usage statistics: cumulative input/output tokens and
    /// cache metrics (if available).
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

        // Render usage statistics on the right side, if available.
        if let Some(usage) = &self.usage {
            let fn_format_num = |n: u64| -> String {
                if n >= 1_000_000 {
                    format!("{}m", n / 1_000_000)
                } else if n >= 1_000 {
                    format!("{}k", n / 1_000)
                } else {
                    n.to_string()
                }
            };

            let mut stats = format!(
                "⟨ in:{} out:{}",
                fn_format_num(usage.input_tokens),
                fn_format_num(usage.output_tokens)
            );

            // Add cache metrics if present
            if usage.cache_read_input_tokens > 0 {
                stats.push_str(&format!(
                    " cr:{}",
                    fn_format_num(usage.cache_read_input_tokens)
                ));
            }
            if usage.cache_creation_input_tokens > 0 {
                stats.push_str(&format!(
                    " cc:{}",
                    fn_format_num(usage.cache_creation_input_tokens)
                ));
            }

            // Add idle time if present
            if let Some(secs) = usage.seconds_since_last_request {
                stats.push_str(&format!(" idle:{}s", secs));
            }

            stats.push_str(" ⟩");

            let stats_width = stats.len() as u16;
            if stats_width < area.width {
                let start_col = area.width.saturating_sub(stats_width);
                for (offset, ch) in stats.chars().enumerate() {
                    let col = start_col as usize + offset;
                    if col < area.width as usize
                        && let Some(cell) = buf.cell_mut((col as u16, area.y))
                    {
                        cell.set_char(ch).set_style(style);
                    }
                }
            }
        }
    }
}
