use ratatui::{buffer::Buffer, layout::Rect};

use super::{render_document::BASELINE_LEFT_MARGIN, state::InteractiveState};

impl<'a> InteractiveState<'a> {
    /// Render the status bar at the bottom of the screen.
    ///
    /// The status message (and streaming indicator if pending), from the left
    /// margin. Usage lives in the header's context meter and the session drawer's
    /// STATS (docs/tui.md).
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
        // slowed loop is always legible.
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
            // What the loop is waiting on (the `current_operations` topic): it leads
            // the transport chips, which qualify it.
            // A pending gate or `ask` owns the slot: the operator is what the loop waits on.
            self.status_operation_text(),
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

        // While the session is believed cold, the cold line leads in the warning weight:
        // what a send, an approval or an answer reads uncached (`cold_line`, docs/tui.md).
        // A session still starting, or one that did not, leads instead: there is no
        // cache to be cold yet.
        let lead = self
            .starting
            .as_ref()
            .map(|starting| starting.line())
            .or_else(|| self.cold_line());
        let lead_len = lead.as_ref().map_or(0, |l| l.chars().count());
        let text = match &lead {
            Some(lead) => format!("{lead}  {text}"),
            None => text,
        };
        let warn = style.add_modifier(ratatui::style::Modifier::BOLD);
        for (i, (col, ch)) in (BASELINE_LEFT_MARGIN..).zip(text.chars()).enumerate() {
            if col >= area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((col, area.y)) {
                cell.set_char(ch)
                    .set_style(if i < lead_len { warn } else { style });
            }
        }
    }
}
