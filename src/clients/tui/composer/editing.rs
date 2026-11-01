//! The composer's key-bound edits: insertion, cursor motion, and deletion.

use super::*;

impl Composer {
    pub(super) fn insert_char(&mut self, c: char) {
        let mut text = [0; 4];
        let text = c.encode_utf8(&mut text);
        let now = Instant::now();
        self.edit(
            CoalescingRun::Typing {
                last_inserted_at: now,
                end_cursor: self.cursor,
            },
            |this| this.insert_text(text),
        );
        self.coalescing_run = CoalescingRun::Typing {
            last_inserted_at: now,
            end_cursor: self.cursor,
        };
    }

    pub(super) fn insert_newline(&mut self) {
        self.edit(CoalescingRun::None, |this| this.insert_text("\n"));
    }

    pub(super) fn insert_tab(&mut self) {
        self.edit(CoalescingRun::None, |this| this.insert_text("\t"));
    }

    pub(super) fn move_left(&mut self) {
        let row = self.cursor_row();
        let line = self.line_range(row);
        if self.cursor > line.start {
            self.cursor = self.previous_grapheme_boundary(self.cursor);
        } else if row > 0 {
            self.cursor = self.line_range(row - 1).end;
        }
    }

    pub(super) fn move_right(&mut self) {
        let row = self.cursor_row();
        let line = self.line_range(row);
        if self.cursor < line.end {
            self.cursor = self.next_grapheme_boundary(self.cursor);
        } else if row + 1 < self.line_starts.len() {
            self.cursor = self.line_range(row + 1).start;
        }
    }

    /// Vertical `↑` with the spill detent (docs/tui.md). Returns `true` if the
    /// motion stayed inside the buffer, `false` if focus should spill into the
    /// transcript. From a lower row it climbs one line preserving the goal column;
    /// on the first line with the caret mid-line it snaps to the start (the `0,0`
    /// detent); only at `0,0` (or empty) does it report a spill.
    pub(super) fn move_up_or_spill(&mut self) -> bool {
        let row = self.cursor_row();
        if row > 0 {
            self.cursor = self.line_offset(row - 1, self.goal_col);
            true
        } else if self.cursor > 0 {
            self.cursor = 0;
            self.goal_col = 0;
            true
        } else {
            false
        }
    }

    pub(super) fn move_down(&mut self) {
        let row = self.cursor_row();
        if row + 1 < self.line_starts.len() {
            self.cursor = self.line_offset(row + 1, self.goal_col);
        }
    }

    /// ASCII letters and digits form a word; all other grapheme clusters separate
    /// words.
    pub(super) fn is_word_grapheme(grapheme: &str) -> bool {
        grapheme.len() == 1 && grapheme.as_bytes()[0].is_ascii_alphanumeric()
    }

    pub(super) fn move_word_left(&mut self) {
        let line_start = self.line_range(self.cursor_row()).start;
        if self.cursor == line_start {
            self.move_left();
            return;
        }
        while self.cursor > line_start {
            let start = self.previous_grapheme_boundary(self.cursor);
            if Self::is_word_grapheme(&self.text[start..self.cursor]) {
                break;
            }
            self.cursor = start;
        }
        while self.cursor > line_start {
            let start = self.previous_grapheme_boundary(self.cursor);
            if !Self::is_word_grapheme(&self.text[start..self.cursor]) {
                break;
            }
            self.cursor = start;
        }
    }

    pub(super) fn move_word_right(&mut self) {
        let line_end = self.line_range(self.cursor_row()).end;
        if self.cursor >= line_end {
            self.move_right();
            return;
        }
        while self.cursor < line_end {
            let end = self.next_grapheme_boundary(self.cursor);
            if Self::is_word_grapheme(&self.text[self.cursor..end]) {
                break;
            }
            self.cursor = end;
        }
        while self.cursor < line_end {
            let end = self.next_grapheme_boundary(self.cursor);
            if !Self::is_word_grapheme(&self.text[self.cursor..end]) {
                break;
            }
            self.cursor = end;
        }
    }

    pub(super) fn delete_back(&mut self) {
        self.edit(CoalescingRun::Backspace, |this| {
            let row = this.cursor_row();
            let line = this.line_range(row);
            if this.cursor > line.start {
                let start = this.previous_grapheme_boundary(this.cursor);
                this.replace_range(start..this.cursor, "");
                this.cursor = start;
            } else if row > 0 {
                let newline = line.start - 1;
                this.replace_range(newline..line.start, "");
                this.cursor = newline;
            }
        });
    }

    pub(super) fn delete_forward(&mut self) {
        self.edit(CoalescingRun::DeleteForward, |this| {
            let row = this.cursor_row();
            let line = this.line_range(row);
            if this.cursor < line.end {
                let end = this.next_grapheme_boundary(this.cursor);
                this.replace_range(this.cursor..end, "");
            } else if row + 1 < this.line_starts.len() {
                this.replace_range(line.end..line.end + 1, "");
            }
        });
    }

    pub(super) fn kill_to_line_end(&mut self) {
        self.edit(CoalescingRun::None, |this| {
            let row = this.cursor_row();
            let line = this.line_range(row);
            if this.cursor < line.end {
                this.replace_range(this.cursor..line.end, "");
            } else if row + 1 < this.line_starts.len() {
                this.replace_range(line.end..line.end + 1, "");
            }
        });
    }

    pub(super) fn kill_to_line_start(&mut self) {
        self.edit(CoalescingRun::None, |this| {
            let start = this.line_range(this.cursor_row()).start;
            this.replace_range(start..this.cursor, "");
            this.cursor = start;
        });
    }

    pub(super) fn delete_word_back(&mut self) {
        self.edit(CoalescingRun::None, |this| {
            let end = this.cursor;
            this.move_word_left();
            let start = this.cursor;
            if start < end {
                this.replace_range(start..end, "");
            }
        });
    }

    /// Delete the next word and any leading separators, retaining following separators.
    pub(super) fn delete_word_forward(&mut self) {
        self.edit(CoalescingRun::None, |this| {
            let start = this.cursor;
            this.move_word_right();
            let end = this.cursor;
            if start < end {
                this.replace_range(start..end, "");
                this.cursor = start;
            }
        });
    }
}
