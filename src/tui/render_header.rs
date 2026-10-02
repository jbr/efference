//! The header row (docs/tui.md): what this session is, how full its context is,
//! and the button that opens the session drawer. The web client's top bar is the
//! model (docs/web-client.md): the wordmark and session name on the left, the
//! context meter and **session** on the right.
//!
//! Chrome, drawn at absolute coordinates. The session button's column span is
//! recorded per frame (`header_hits`) for click dispatch, like the strip's items.

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::render_sidebar::num;
use super::state::InteractiveState;
use super::write_text::fold_control;

/// A clickable item on the header row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderItem {
    /// Opens (or closes) the session drawer: details, stats, processes, policy.
    Session,
}

/// Cells the context meter's bar takes.
const METER_CELLS: u64 = 10;

/// Write `text` at `(x, y)` in `style`, one cell per char, stopping at `limit`.
/// Returns the next free column.
fn put(buf: &mut Buffer, x: u16, y: u16, limit: u16, text: &str, style: Style) -> u16 {
    let mut col = x;
    for ch in text.chars() {
        if col >= limit {
            break;
        }
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_char(fold_control(ch)).set_style(style);
        }
        col += 1;
    }
    col
}

impl<'a> InteractiveState<'a> {
    pub(super) fn render_header(&mut self, buf: &mut Buffer, area: Rect) {
        let bg = self.theme.document_bg_style;
        let muted = self.theme.muted_style;
        let y = area.y;
        let right_edge = area.x + area.width;
        for x in area.x..right_edge {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(bg);
            }
        }
        self.viewport.header_hits.clear();

        // Right side first, so the left side knows where it must stop: the session
        // button, then (leftward) cost, the context meter, and the cold-cache warning.
        let mut segments: Vec<(String, Style)> = Vec::new();
        if let Some(u) = &self.usage {
            if u.cold_streak >= crate::daemon::COLD_STREAK_WARN {
                segments.push((
                    format!("⚠ cache cold ×{}", u.cold_streak),
                    bg.fg(self.theme.status_style.fg.unwrap_or_default())
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ));
            }
            if let Some(limit) = u.context_limit.filter(|l| *l > 0) {
                let used = u.context_used.unwrap_or(0);
                let filled = (used * METER_CELLS).div_ceil(limit).min(METER_CELLS);
                let filled = if used > 0 { filled.max(1) } else { 0 };
                let accent = self.theme.strip_item_focused_style.fg;
                segments.push((
                    "━".repeat(filled as usize),
                    bg.fg(accent.unwrap_or_default()),
                ));
                segments.push(("─".repeat((METER_CELLS - filled) as usize), muted));
                let caption = match u.context_used {
                    Some(used) => format!(" {}/{}", num(used), num(limit)),
                    None => format!(" {}", num(limit)),
                };
                segments.push((caption, muted));
            }
            if let Some(usd) = u.cost_usd {
                segments.push((format!("  ${usd:.2}"), muted));
            }
        }

        let label = " session ";
        let button_style =
            if self.sidebar_open || self.viewport.header_hovered == Some(HeaderItem::Session) {
                self.theme.strip_item_focused_style
            } else {
                self.theme.strip_item_style
            };
        let button_w = label.chars().count() as u16;
        let button_x = right_edge.saturating_sub(button_w + 1);
        let segments_w: u16 = segments
            .iter()
            .map(|(text, _)| text.chars().count() as u16)
            .sum();
        // Two columns between the meter group and the button, as between strip items.
        let mut x = button_x.saturating_sub(segments_w + 2);
        let left_limit = x.saturating_sub(2);
        for (text, style) in &segments {
            x = put(buf, x, y, button_x, text, *style);
        }
        put(buf, button_x, y, right_edge, label, button_style);
        self.viewport
            .header_hits
            .push((button_x, button_x + button_w, HeaderItem::Session));

        // Left: the wordmark, then the session's name and title.
        // The status style's foreground on the header's own background: the
        // status style carries the status row's band, which does not belong here.
        let text = bg.fg(self.theme.status_style.fg.unwrap_or_default());
        let mut x = put(
            buf,
            area.x + 1,
            y,
            left_limit,
            "efference",
            text.add_modifier(ratatui::style::Modifier::BOLD),
        );
        if let Some(identity) = &self.session_identity {
            x = put(buf, x + 2, y, left_limit, &identity.id, muted);
            if let Some(title) = &identity.title {
                x = put(buf, x, y, left_limit, " · ", muted);
                put(buf, x, y, left_limit, title, text);
            }
        }
    }
}
