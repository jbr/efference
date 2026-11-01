//! The help overlay: every keybinding the TUI answers (docs/tui.md).

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::state::InteractiveState;
use super::theme::InteractiveTheme;

impl<'a> InteractiveState<'a> {
    /// Render help screen showing all available keybindings
    pub(super) fn render_help_screen(&mut self, buf: &mut Buffer, area: Rect) {
        let bg_style = self.theme.help_bg_style;

        // Clear the entire screen
        for y in 0..area.height {
            for x in 0..area.width {
                buf.cell_mut((x, y)).unwrap().reset();
                buf.cell_mut((x, y)).unwrap().set_style(bg_style);
            }
        }

        let help_text = help_rows(&self.theme);

        // Calculate maximum width for consistent formatting
        let max_width = help_text
            .iter()
            .map(|(key, desc, _)| {
                if key.is_empty() {
                    desc.len()
                } else {
                    format!("{key:20} {desc}").len()
                }
            })
            .max()
            .unwrap_or(60);

        let start_row = (area.height.saturating_sub(help_text.len() as u16)) / 2;
        let start_col = (area.width.saturating_sub(max_width as u16)) / 2;

        for (i, (key, desc, style)) in help_text.iter().enumerate() {
            let row = start_row + i as u16;
            if row >= area.height {
                break;
            }

            let text = if key.is_empty() {
                format!("{desc:max_width$}")
            } else {
                format!("{:20} {:width$}", key, desc, width = max_width - 21)
            };

            for (col, ch) in (start_col..).zip(text.chars()) {
                if col >= area.width {
                    break;
                }
                buf.cell_mut((col, row))
                    .unwrap()
                    .set_char(super::write_text::fold_control(ch))
                    .set_style(*style);
            }
        }
    }
}

/// The help screen's rows, in the order shown: the key column, the description, and the
/// style the row draws in.
fn help_rows(theme: &InteractiveTheme) -> Vec<(&'static str, &'static str, Style)> {
    let bg_style = theme.help_bg_style;
    let title_style = theme.help_title_style;
    let key_style = theme.help_key_style;
    let desc_style = theme.help_desc_style;
    vec![
        ("", "EFFERENCE — TUI KEYBINDINGS", title_style),
        ("", "", bg_style),
        ("Navigation:", "", title_style),
        ("  j, ↓, Ctrl+n", "Scroll down", key_style),
        ("  k, ↑, Ctrl+p", "Scroll up", key_style),
        ("  Ctrl+d, PgDn", "Page down", key_style),
        ("  Ctrl+u, PgUp", "Page up", key_style),
        ("  g, Home", "Jump to top", key_style),
        (
            "  Shift+G, End",
            "Jump to bottom (follow stream)",
            key_style,
        ),
        ("", "", bg_style),
        ("Focused block (block menu):", "", title_style),
        ("  Shift+Tab", "Move focus up a block", key_style),
        ("  ← , →", "Choose among the block's verbs", key_style),
        (
            "  Enter, Space",
            "Fire it — raw / triaged, or full / peek",
            key_style,
        ),
        ("", "", bg_style),
        ("Control strip (↑ from the composer):", "", title_style),
        (
            "  ↑ , then ← / →",
            "Reach the strip, then choose a verb",
            key_style,
        ),
        (
            "  Enter, Click",
            "Fire it — pause, pace, background, theme…",
            key_style,
        ),
        (
            "  letter",
            "A shown verb's key fires it (e.g. b: background)",
            key_style,
        ),
        ("", "", bg_style),
        ("Sidebar (Tab from command mode):", "", title_style),
        ("  Tab", "Toggle into / out of the sidebar", key_style),
        (
            "  ↑ , ↓",
            "Move between rows (processes, proposals)",
            key_style,
        ),
        ("  ← , →", "Choose among the row's verbs", key_style),
        (
            "  Enter, Click",
            "Fire it — kill a process; commit or cancel a proposal",
            key_style,
        ),
        ("  Esc", "Back to the composer", key_style),
        ("", "", bg_style),
        ("Pace & interrupt:", "", title_style),
        (
            "  Esc",
            "Composer: pause the loop; again: stop the tool",
            key_style,
        ),
        ("", "", bg_style),
        ("Mouse:", "", title_style),
        ("  m", "Toggle mouse mode (for text selection)", key_style),
        (
            "  Click",
            "Focus a block, or fire one of its verbs",
            key_style,
        ),
        ("  Scroll", "Scroll content", key_style),
        ("", "", bg_style),
        ("Help:", "", title_style),
        ("  ?, h", "Show this help screen", key_style),
        ("  Esc", "Close help / theme picker", key_style),
        ("", "", bg_style),
        ("Other:", "", title_style),
        ("  q, Ctrl+c", "Quit", key_style),
        ("", "", bg_style),
        ("", "Press any key to close help", desc_style),
    ]
}
