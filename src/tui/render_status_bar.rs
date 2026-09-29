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
        // turn gets a marker prefix, and a non-default pace gets a loud `⏸`/`⏱`
        // indicator ahead of everything (docs/tui.md) so a held or
        // slowed loop is always legible, even when the sidebar owns the right side.
        // While a question is pending, the idle hint says which one a send answers
        // (docs/tui.md); a message about something the operator just did keeps its place.
        let message = match &self.ask {
            Some(ask)
                if matches!(
                    &*self.ui.debug_message,
                    super::state::IDLE_STATUS | "click to focus this entry"
                ) =>
            {
                if ask.of > 1 {
                    format!("your reply answers question {} of {}", ask.n, ask.of)
                } else {
                    "your reply answers the question".to_string()
                }
            }
            _ => self.ui.debug_message.to_string(),
        };
        let base = if self.loading.pending_request {
            format!("◍ streaming…  {message}")
        } else {
            message
        };
        // The frame-cost readout leads, when `--frame-cost` asked for it: it is being
        // read *while* something streams, so it must not be the thing that gets
        // truncated when the status message is long.
        let indicators = [
            self.frame_cost.as_ref().and_then(|c| c.readout()),
            super::state::pace_indicator(self.pace),
            // The retry leads the failure chip: it is the live state, and it says what
            // is being done about the status the chip names (docs/transport.md).
            super::state::upstream_retry_indicator(self.upstream_retry.as_ref()),
            super::state::upstream_failure_indicator(self.upstream_failure),
            // The stall leads nothing and trails everything: it is the terminal
            // condition, true until a turn starts, where the two above are live
            // transport state (docs/transport.md).
            super::state::stall_indicator(self.stall.as_ref()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let text = if indicators.is_empty() {
            base
        } else {
            format!("{}  {base}", indicators.join("  "))
        };

        for (col, ch) in (BASELINE_LEFT_MARGIN..).zip(text.chars()) {
            if col >= area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((col, area.y)) {
                cell.set_char(ch).set_style(style);
            }
        }

        // Render usage statistics on the right side, if available — but only when the
        // sidebar isn't docked. When it is, STATS lives there (docs/tui.md); the status
        // bar carries usage solely as the narrow-terminal fallback.
        if self.viewport.sidebar_area.is_none()
            && let Some(usage) = &self.usage
        {
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
