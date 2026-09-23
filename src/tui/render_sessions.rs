//! Painting the session switcher (docs/tui.md).
//!
//! A plain centred list: one row per session, the status and process count the
//! listing carries, and a note line. The keys it teaches are the ones
//! `sessions.rs` implements.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Modifier,
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Widget},
};

use super::sessions::SessionMenu;
use super::state::InteractiveState;

impl InteractiveState<'_> {
    pub(super) fn render_sessions(&mut self, buf: &mut Buffer, area: Rect, menu: &SessionMenu) {
        // A modal blocks what is under it, so the frame's document actions go
        // with it — the same rule the theme picker follows.
        self.render_cache.actions.clear();

        // The form replaces the listing rather than sitting beside it: it owns
        // every key while it is open, so showing the rows it cannot act on
        // would only advertise keys that no longer do anything.
        if let Some(form) = &menu.form {
            let width = area.width.min(92);
            let height = area.height.min(form.viewport_height() + 2);
            let modal = Rect::new(
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
                width,
                height,
            );
            Clear.render(modal, buf);
            let border = Block::default()
                .borders(Borders::ALL)
                .style(self.theme.help_bg_style);
            let inner = border.inner(modal);
            border.render(modal, buf);
            form.render(
                buf,
                inner,
                super::session_form::render::Styles::from(&self.theme),
            );
            return;
        }
        let modal = centered(area);
        Clear.render(modal, buf);

        let here = self.session_identity.as_ref().map(|identity| &identity.id);
        let items: Vec<ListItem> = menu
            .rows
            .iter()
            .map(|row| {
                let title = row.title.as_deref().unwrap_or("untitled");
                let mut line = format!("{}  {}  {}", row.id, row.status, title);
                if row.processes > 0 {
                    line.push_str(&format!("  · {} bg", row.processes));
                }
                if Some(&row.id) == here {
                    line.push_str("  · attached");
                }
                ListItem::new(Line::from(format!("  {line}")))
            })
            .collect();

        let mut list_state = ListState::default();
        if !menu.rows.is_empty() {
            list_state.select(Some(menu.selected.min(menu.rows.len() - 1)));
        }
        let list = List::new(items)
            .block(
                Block::default()
                    .title(" sessions ")
                    .borders(Borders::ALL)
                    .style(self.theme.help_bg_style),
            )
            .highlight_style(self.theme.breadcrumb_style.add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        ratatui::widgets::StatefulWidget::render(list, modal, buf, &mut list_state);

        // The note, then the key legend, on the modal's last two rows.
        let note = menu.note.clone().unwrap_or_default();
        let legend = match menu.confirming {
            Some(_) => " y:end  any other key:cancel ",
            None => " ↑/↓:move  ⏎:switch  n:new  e:end  ⎋:close ",
        };
        write_row(buf, modal, 3, &note, self.theme.status_hint_style);
        write_row(buf, modal, 2, legend, self.theme.status_hint_style);
    }
}

/// Write `text` into the modal's row `up` lines above its bottom border,
/// clipped to the inner width.
fn write_row(buf: &mut Buffer, modal: Rect, up: u16, text: &str, style: ratatui::style::Style) {
    if modal.height <= up || text.is_empty() {
        return;
    }
    let y = modal.y + modal.height - up;
    let inner = modal.width.saturating_sub(2) as usize;
    for (i, ch) in text.chars().take(inner).enumerate() {
        if let Some(cell) = buf.cell_mut((modal.x + 1 + i as u16, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// The modal's area: 60% × 60% of the screen, centred.
fn centered(area: Rect) -> Rect {
    let rows = Layout::vertical([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .split(rows[1])[1]
}
