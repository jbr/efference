//! The session drawer (docs/tui.md): Session, Processes, Usage, and Policy, as
//! labelled rows the way the web client's drawer lays them out. It is chrome, not
//! transcript: rendered directly into its `Rect` at absolute coordinates, fed by
//! the `state` topics (docs/control-api.md), over the transcript's right side
//! while open. A 1-column `│` divides it from the transcript on its left edge.
//!
//! Sections render top-down until they run out of room. Row and verb rects are
//! recorded per frame for click dispatch (`sidebar_hits` / `sidebar_verb_hits`); the
//! verbs themselves dispatch by identity through `tui/sidebar.rs`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::SidebarRow;
use super::policy_view::PolicyView;
use super::state::InteractiveState;
use super::write_text::fold_control;

mod sections;

/// Write `text` at absolute `(x, y)`, clipped to `max_w` columns. One cell per char
/// (wide glyphs occupy one cell, as elsewhere in this TUI's chrome). Content text
/// lands here too — a process's command line is whatever the model wrote, newlines
/// included — so control characters are folded, not written.
fn put(buf: &mut Buffer, x: u16, y: u16, max_w: u16, text: &str, style: Style) {
    for (i, ch) in text.chars().take(max_w as usize).enumerate() {
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(fold_control(ch)).set_style(style);
        }
    }
}

/// Columns of the label gutter: values start this far into the content.
const LABEL_W: u16 = 11;

/// Where the drawer draws and in which styles, derived from its rect.
#[derive(Clone, Copy)]
struct Panel {
    area: Rect,
    /// Content start: past the divider and one space.
    x0: u16,
    /// Content width, less a trailing pad.
    max_w: u16,
    /// The value column, past the label gutter.
    vx: u16,
    /// The value column's width.
    vw: u16,
    bottom: u16,
    text: Style,
    muted: Style,
    header: Style,
}

impl Panel {
    /// One `label  value` row: the label muted in its gutter, the value beside it.
    fn kv(&self, buf: &mut Buffer, y: u16, label: &str, value: &str, style: Style) {
        put(buf, self.x0, y, LABEL_W, label, self.muted);
        put(buf, self.vx, y, self.vw, value, style);
    }
}

impl<'a> InteractiveState<'a> {
    pub(super) fn render_sidebar(&mut self, buf: &mut Buffer, area: Rect) {
        let bg = self.theme.document_bg_style;
        let muted = self.theme.muted_style;
        // The status style's foreground on the drawer's own background: the status
        // style also carries the status row's band, which does not belong here.
        let text = bg.fg(self.theme.status_style.fg.unwrap_or_default());
        let header = text.add_modifier(Modifier::BOLD);

        // Clear the panel and paint the left divider column.
        for dy in 0..area.height {
            for dx in 0..area.width {
                if let Some(cell) = buf.cell_mut((area.x + dx, area.y + dy)) {
                    cell.reset();
                    cell.set_style(bg);
                }
            }
            if let Some(cell) = buf.cell_mut((area.x, area.y + dy)) {
                cell.set_char('│').set_style(muted);
            }
        }

        // Hits are rebuilt with the frame, like the strip's and block menu's.
        self.sidebar_hits.clear();
        self.sidebar_verb_hits.clear();

        let x0 = area.x + 2; // content start: past the divider + one space
        let max_w = area.width.saturating_sub(3); // trailing pad
        let panel = Panel {
            area,
            x0,
            max_w,
            vx: x0 + LABEL_W, // the value column
            vw: max_w.saturating_sub(LABEL_W),
            bottom: area.y + area.height,
            text,
            muted,
            header,
        };

        // Owned snapshots for the render walk: the verb menus record hits through
        // `&mut self`, which cannot run while the topics are borrowed.
        let processes = self.process_panel.clone();
        let policy = PolicyView::of(&self.topics);

        let mut y = area.y;
        y = self.render_session_section(buf, &panel, y);
        y = self.render_processes_section(buf, &panel, y, &processes);
        y = self.render_usage_section(buf, &panel, y);
        self.render_policy_section(buf, &panel, y, policy.as_ref());
    }

    /// One actionable POLICY line: the label, plus — when it is the focused row —
    /// the `▌` bar and the verb menu trailing it, truncated from the label side so
    /// the verbs (what `⏎` fires) always render whole. Records the row hit rect.
    /// Content geometry (`x0`, `max_w`) derives from the panel rect, as everywhere
    /// else in this module.
    fn render_policy_line(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        label: &str,
        row: SidebarRow,
        style: Style,
    ) {
        let x0 = area.x + 2 + LABEL_W;
        let max_w = area.width.saturating_sub(3 + LABEL_W);
        let focused = self.sidebar_focus == Some(row);
        // Actionable rows are clickable focused or not — the click is how a row
        // *becomes* focused.
        self.sidebar_hits.push((Rect::new(x0, y, max_w, 1), row));
        if !focused {
            put(buf, x0, y, max_w, label, style);
            return;
        }
        self.paint_focus_bar(buf, area.x, y, 1);
        // The menu's width claims the line first; the label gets what remains.
        let menu_w: u16 = self
            .sidebar_verbs(row)
            .iter()
            .map(|v| 3 + v.label().chars().count() as u16 + 1)
            .sum();
        let label_room = max_w.saturating_sub(menu_w + 1);
        put(buf, x0, y, label_room, label, style);
        let tx = x0 + label_room + 1;
        self.render_row_verbs(buf, tx, y, area.x + area.width, row);
    }

    /// The focused row's left-edge bar: a `▌` per line in the pad column between
    /// the divider and the content — the transcript unit marker, one column over.
    fn paint_focus_bar(&self, buf: &mut Buffer, x: u16, y: u16, height: u16) {
        for dy in 0..height {
            if let Some(cell) = buf.cell_mut((x + 1, y + dy)) {
                cell.set_char('▌')
                    .set_style(self.theme.sidebar_focus_bar_style);
            }
        }
    }

    /// A focused row's verb menu: each verb as `▸ label` with a reserved marker
    /// cell (the block menu's grammar), the selected one in the strip's focused
    /// style. Records each verb's rect for click dispatch; verbs cut from the
    /// right at the panel edge are simply not offered — the `←`/`→` selection
    /// clamps to what is on screen.
    fn render_row_verbs(
        &mut self,
        buf: &mut Buffer,
        mut x: u16,
        y: u16,
        max_x: u16,
        row: SidebarRow,
    ) {
        let selected = self.sidebar_verb_focus;
        for (i, verb) in self.sidebar_verbs(row).into_iter().enumerate() {
            let text = format!(
                "{} {}",
                if i == selected { "▸" } else { "  " },
                verb.label()
            );
            let w = text.chars().count() as u16;
            if x + w > max_x {
                break;
            }
            let style = if i == selected {
                self.theme.strip_item_focused_style
            } else {
                self.theme.strip_item_style
            };
            put(buf, x, y, w, &text, style);
            self.sidebar_verb_hits.push((Rect::new(x, y, w, 1), row, i));
            x += w + 1;
        }
    }
}

/// Compact a token count: `12k` / `1m` / `486`.
pub(super) fn num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}m", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}
