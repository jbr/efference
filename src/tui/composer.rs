//! The input composer — a minimal, dependency-free multiline text editor with
//! emacs-style key bindings (DESIGN §10.5, M2).
//!
//! This is deliberately self-contained behind a small interface
//! ([`handle_key`](Composer::handle_key) / [`render`](Composer::render) /
//! [`take`](Composer::take)) so it is the **swappable input seam**: a later
//! version can grow spellcheck, typeahead, or integrated transcript-focus (arrow
//! at the top edge spills into scroll), or be replaced wholesale, without
//! touching the rest of the TUI. `tui-textarea` was the intended first cut but is
//! pinned to the older ratatui/unicode-width ecosystem and won't resolve against
//! ratatui 0.30 — hence this small hand-rolled editor.
//!
//! The buffer is `Vec<Vec<char>>` (one row per line) so cursor columns are plain
//! char indices — correct across UTF-8 without byte-boundary juggling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Position, layout::Rect, style::Style};
use unicode_width::UnicodeWidthChar;

/// What feeding a key to the composer produced.
pub(super) enum ComposerAction {
    /// The key was consumed (edit/move) or ignored — nothing for the caller.
    Consumed,
    /// Not a composer key — the caller should handle it (e.g. scroll, quit).
    Ignored,
    /// The user submitted this text (Enter on a non-empty buffer).
    Submit(String),
}

#[derive(Debug)]
pub(super) struct Composer {
    /// One entry per line; never empty (always at least one, possibly-empty line).
    lines: Vec<Vec<char>>,
    /// Cursor line index.
    row: usize,
    /// Cursor column as a char index within `lines[row]`.
    col: usize,
    /// Preferred column for vertical motion (emacs "goal column").
    goal_col: usize,
}

impl Composer {
    pub(super) fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            goal_col: 0,
        }
    }

    /// True when the buffer holds no non-whitespace text.
    pub(super) fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.iter().all(|c| c.is_whitespace()))
    }

    /// The number of text rows (for sizing the input area).
    pub(super) fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The current text (lines joined with `\n`).
    pub(super) fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Reset to an empty single line.
    pub(super) fn clear(&mut self) {
        self.lines = vec![Vec::new()];
        self.row = 0;
        self.col = 0;
        self.goal_col = 0;
    }

    /// Feed one key event. Returns whether it was consumed, ignored (caller should
    /// handle), or a submission.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // Submit on plain Enter; insert a newline on C-j / Alt-Enter (Enter is
            // ambiguous for newline across terminals, so the explicit keys win).
            KeyCode::Enter if alt => self.insert_newline(),
            KeyCode::Enter => {
                if self.is_empty() {
                    return ComposerAction::Consumed;
                }
                return ComposerAction::Submit(self.text());
            }
            // C-j is a literal newline — the reliable multiline key.
            KeyCode::Char('j') if ctrl => self.insert_newline(),

            // Emacs motion.
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = self.line_len(),
            KeyCode::Char('b') if ctrl => self.move_left(),
            KeyCode::Char('f') if ctrl => self.move_right(),
            KeyCode::Char('p') if ctrl => self.move_up(),
            KeyCode::Char('n') if ctrl => self.move_down(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(),
            KeyCode::Char('b') if alt => self.move_word_left(),
            KeyCode::Char('f') if alt => self.move_word_right(),

            // Emacs editing.
            KeyCode::Backspace => self.delete_back(),
            KeyCode::Char('h') if ctrl => self.delete_back(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('d') if ctrl => self.delete_forward(),
            KeyCode::Char('k') if ctrl => self.kill_to_line_end(),
            KeyCode::Char('u') if ctrl => self.kill_to_line_start(),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),

            // Printable insertion (ignore other control combos so they fall
            // through to the caller as commands).
            KeyCode::Char(c) if !ctrl && !alt => self.insert_char(c),
            KeyCode::Char(' ') => self.insert_char(' '),
            KeyCode::Tab => self.insert_char('\t'),

            // Anything else (PgUp/PgDn, F-keys, unbound Ctrl combos) belongs to the
            // caller.
            _ => return ComposerAction::Ignored,
        }
        // Motion that changed the column should update the goal column; the
        // vertical-motion helpers manage goal_col themselves.
        if !matches!(key.code, KeyCode::Up | KeyCode::Down)
            && !(ctrl && matches!(key.code, KeyCode::Char('p') | KeyCode::Char('n')))
        {
            self.goal_col = self.col;
        }
        ComposerAction::Consumed
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].len()
    }

    fn insert_char(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
    }

    fn insert_newline(&mut self) {
        let tail = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_len();
        }
    }

    fn move_right(&mut self) {
        if self.col < self.line_len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.goal_col.min(self.line_len());
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.goal_col.min(self.line_len());
        }
    }

    fn move_word_left(&mut self) {
        let line = &self.lines[self.row];
        if self.col == 0 {
            self.move_left();
            return;
        }
        let mut c = self.col;
        while c > 0 && line[c - 1].is_whitespace() {
            c -= 1;
        }
        while c > 0 && !line[c - 1].is_whitespace() {
            c -= 1;
        }
        self.col = c;
    }

    fn move_word_right(&mut self) {
        let len = self.line_len();
        if self.col >= len {
            self.move_right();
            return;
        }
        let line = &self.lines[self.row];
        let mut c = self.col;
        while c < len && line[c].is_whitespace() {
            c += 1;
        }
        while c < len && !line[c].is_whitespace() {
            c += 1;
        }
        self.col = c;
    }

    fn delete_back(&mut self) {
        if self.col > 0 {
            self.lines[self.row].remove(self.col - 1);
            self.col -= 1;
        } else if self.row > 0 {
            // Merge with the previous line.
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].extend(cur);
        }
    }

    fn delete_forward(&mut self) {
        if self.col < self.line_len() {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        }
    }

    fn kill_to_line_end(&mut self) {
        if self.col < self.line_len() {
            self.lines[self.row].truncate(self.col);
        } else if self.row + 1 < self.lines.len() {
            // At end of line: pull up the next line.
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        }
    }

    fn kill_to_line_start(&mut self) {
        self.lines[self.row].drain(0..self.col);
        self.col = 0;
    }

    fn delete_word_back(&mut self) {
        let start = self.col;
        self.move_word_left();
        let end = self.col;
        if end < start {
            self.lines[self.row].drain(end..start);
        }
    }

    /// Render the composer into `area`, returning the on-screen cursor position so
    /// the caller can place the terminal cursor when the composer is focused.
    /// Long buffers scroll so the cursor row stays visible.
    pub(super) fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        text_style: Style,
        prompt_style: Style,
    ) -> Position {
        if area.width == 0 || area.height == 0 {
            return Position::new(area.x, area.y);
        }
        let prompt = "› ";
        let prompt_w = prompt.chars().count() as u16;

        // Vertical scroll within the input area so the cursor row is visible.
        let visible = area.height as usize;
        let top = self.row.saturating_sub(visible.saturating_sub(1));

        for (screen_row, line_idx) in (top..self.lines.len()).take(visible).enumerate() {
            let y = area.y + screen_row as u16;
            let mut x = area.x;
            // Prompt marker on every wrapped row's left gutter.
            for ch in prompt.chars() {
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch).set_style(prompt_style);
                }
                x += 1;
            }
            for &ch in &self.lines[line_idx] {
                let w = ch.width().unwrap_or(0) as u16;
                if x + w > area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch).set_style(text_style);
                }
                x += w.max(1);
            }
        }

        // Cursor screen position: prompt + display width up to the cursor column.
        let cursor_screen_row = self.row.saturating_sub(top) as u16;
        let mut cursor_x = area.x + prompt_w;
        for &ch in &self.lines[self.row][..self.col] {
            cursor_x += ch.width().unwrap_or(0).max(1) as u16;
        }
        Position::new(cursor_x.min(area.right().saturating_sub(1)), area.y + cursor_screen_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn types_and_submits() {
        let mut c = Composer::new();
        for ch in "hello".chars() {
            assert!(matches!(c.handle_key(key(ch)), ComposerAction::Consumed));
        }
        assert_eq!(c.text(), "hello");
        match c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ComposerAction::Submit(t) => assert_eq!(t, "hello"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn empty_enter_does_not_submit() {
        let mut c = Composer::new();
        assert!(matches!(
            c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ComposerAction::Consumed
        ));
    }

    #[test]
    fn ctrl_j_inserts_newline_and_text_joins() {
        let mut c = Composer::new();
        for ch in "ab".chars() {
            c.handle_key(key(ch));
        }
        c.handle_key(ctrl('j'));
        for ch in "cd".chars() {
            c.handle_key(key(ch));
        }
        assert_eq!(c.text(), "ab\ncd");
        assert_eq!(c.line_count(), 2);
    }

    #[test]
    fn emacs_motion_and_kill() {
        let mut c = Composer::new();
        for ch in "hello world".chars() {
            c.handle_key(key(ch));
        }
        c.handle_key(ctrl('a')); // line start
        c.handle_key(ctrl('k')); // kill to end
        assert_eq!(c.text(), "");
    }

    #[test]
    fn ctrl_w_deletes_word_back() {
        let mut c = Composer::new();
        for ch in "foo bar".chars() {
            c.handle_key(key(ch));
        }
        c.handle_key(ctrl('w'));
        assert_eq!(c.text(), "foo ");
    }

    #[test]
    fn backspace_merges_lines() {
        let mut c = Composer::new();
        c.handle_key(key('a'));
        c.handle_key(ctrl('j'));
        c.handle_key(key('b'));
        // cursor after 'b' on line 2; move to line start then backspace merges.
        c.handle_key(ctrl('a'));
        c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(c.text(), "ab");
        assert_eq!(c.line_count(), 1);
    }

    #[test]
    fn non_composer_keys_are_ignored() {
        let mut c = Composer::new();
        assert!(matches!(
            c.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            ComposerAction::Ignored
        ));
    }
}
