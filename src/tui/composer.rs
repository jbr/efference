//! The input composer — a minimal, dependency-free multiline text editor with
//! emacs-style key bindings (docs/tui.md, M2).
//!
//! This is deliberately self-contained behind a small interface
//! ([`handle_key`](Composer::handle_key) / [`render`](Composer::render) /
//! [`take`](Composer::take)) so it is the **swappable input interface**: a later
//! version can grow spellcheck, typeahead, or integrated transcript-focus (arrow
//! at the top edge spills into scroll), or be replaced wholesale, without
//! touching the rest of the TUI. `tui-textarea` was the intended first cut but is
//! pinned to the older ratatui/unicode-width ecosystem and won't resolve against
//! ratatui 0.30 — hence this small hand-rolled editor.
//!
//! The buffer is one UTF-8 string. Cursor and layout positions are byte offsets
//! constrained to extended-grapheme boundaries, so edits cannot split a
//! user-perceived character.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Position, layout::Rect, style::Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::render_span::wrap_points;

/// What feeding a key to the composer produced.
pub(super) enum ComposerAction {
    /// The key was consumed (edit/move) or ignored — nothing for the caller.
    Consumed,
    /// Not a composer key — the caller should handle it (e.g. scroll, quit).
    Ignored,
    /// The user submitted this text (Enter on a non-empty buffer).
    Submit(String),
    /// `↑` at the very top of the buffer (`0,0`, or an empty buffer): focus should
    /// spill out the composer's top edge into the transcript (docs/tui.md). The
    /// caller crosses the boundary; the composer keeps its text. The `0,0` detent
    /// (one keypress lands at the start of the first line before the *next* `↑`
    /// spills) is handled inside the composer — see [`move_up_or_spill`](Composer::move_up_or_spill).
    SpillUp,
}

/// The left-gutter prompt, drawn on every visual row. Its width is the chrome
/// the wrappable content width is measured against (see [`Composer::content_width`]).
const PROMPT: &str = "› ";

/// One wrapped row of the composer: a byte range `[start, end)` within logical
/// line `line`. Produced by [`Composer::visual_rows`].
#[derive(Debug, Clone, Copy)]
struct VisualRow {
    line: usize,
    start: usize,
    end: usize,
}

#[derive(Debug)]
pub(super) struct Composer {
    /// UTF-8 text for all logical lines, separated by `\n`.
    text: String,
    /// Byte offset of each logical line's first byte; always starts with zero.
    line_starts: Vec<usize>,
    /// Byte offset in `text`, always an extended-grapheme boundary.
    cursor: usize,
    /// Fixed endpoint of an in-progress selection. `None` means no selection.
    /// When equal to `cursor`, it is normalized back to `None`.
    anchor: Option<usize>,
    /// Preferred grapheme column for vertical motion (emacs "goal column").
    goal_col: usize,
}

impl Composer {
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            line_starts: vec![0],
            cursor: 0,
            anchor: None,
            goal_col: 0,
        }
    }

    /// True when the buffer holds no non-whitespace text.
    pub(super) fn is_empty(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }

    /// The current logical line and its byte range, excluding its trailing newline.
    fn line_range(&self, row: usize) -> Range<usize> {
        let start = self.line_starts[row];
        let end = self
            .line_starts
            .get(row + 1)
            .map_or(self.text.len(), |next| next - 1);
        start..end
    }

    fn cursor_row(&self) -> usize {
        self.line_starts
            .partition_point(|&start| start <= self.cursor)
            - 1
    }

    fn cursor_col(&self) -> usize {
        let range = self.line_range(self.cursor_row());
        self.text[range.start..self.cursor].graphemes(true).count()
    }

    fn line_offset(&self, row: usize, col: usize) -> usize {
        let range = self.line_range(row);
        self.text[range.clone()]
            .grapheme_indices(true)
            .nth(col)
            .map_or(range.end, |(offset, _)| range.start + offset)
    }

    fn previous_grapheme_boundary(&self, offset: usize) -> usize {
        self.text[..offset]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start)
    }

    fn next_grapheme_boundary(&self, offset: usize) -> usize {
        self.text[offset..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.text.len(), |(end, _)| offset + end)
    }

    fn replace_range(&mut self, range: Range<usize>, replacement: &str) {
        let old_len = range.end - range.start;
        let delta = replacement.len() as isize - old_len as isize;
        let added_line_starts: Vec<_> = replacement
            .bytes()
            .enumerate()
            .filter_map(|(offset, byte)| (byte == b'\n').then_some(range.start + offset + 1))
            .collect();

        self.text.replace_range(range.clone(), replacement);
        self.line_starts
            .retain(|&start| start <= range.start || start > range.end);
        for start in self
            .line_starts
            .iter_mut()
            .filter(|start| **start > range.start)
        {
            *start = start.saturating_add_signed(delta);
        }
        self.line_starts.extend(added_line_starts);
        self.line_starts.sort_unstable();
        debug_assert_eq!(
            self.line_starts.len(),
            self.text.bytes().filter(|&b| b == b'\n').count() + 1
        );
    }

    fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// The selected plain text, if the anchor and cursor enclose a range.
    pub(super) fn selected_text(&self) -> Option<String> {
        self.selection_range()
            .map(|range| self.text[range].to_string())
    }

    /// Delete the selected text, returning it for a TUI-owned clipboard action.
    pub(super) fn cut_selection(&mut self) -> Option<String> {
        let range = self.selection_range()?;
        let text = self.text[range.clone()].to_string();
        self.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.clear_selection();
        self.goal_col = self.cursor_col();
        Some(text)
    }

    /// Insert externally supplied plain text, replacing a selection when present.
    pub(super) fn paste_text(&mut self, text: &str) {
        self.insert_text(text);
        self.goal_col = self.cursor_col();
    }

    /// Position the cursor at a byte offset already known to be a grapheme
    /// boundary. A mouse press either clears or starts a selection; a drag keeps
    /// the press position as its anchor.
    pub(super) fn set_cursor_from_mouse(&mut self, cursor: usize, extend: bool) {
        debug_assert!(self.text.is_char_boundary(cursor));
        if extend {
            self.extend_selection();
        } else {
            self.anchor = Some(cursor);
        }
        self.cursor = cursor;
        if self.anchor == Some(self.cursor) && !extend {
            self.clear_selection();
        }
        self.goal_col = self.cursor_col();
    }

    /// Update the active mouse drag, preserving its press position as the anchor.
    pub(super) fn extend_selection_from_mouse(&mut self, cursor: usize) {
        debug_assert!(self.text.is_char_boundary(cursor));
        self.extend_selection();
        self.cursor = cursor;
        if self.anchor == Some(self.cursor) {
            self.clear_selection();
        }
        self.goal_col = self.cursor_col();
    }

    /// Map a screen cell in the rendered composer text rectangle to the closest
    /// grapheme boundary. Rows and wrapping are shared with rendering.
    pub(super) fn cursor_for_screen_position(&self, area: Rect, column: u16, row: u16) -> usize {
        let rows = self.visual_rows(Self::content_width(area.width));
        if rows.is_empty() {
            return 0;
        }
        let (cursor_vr, _) = self.locate_cursor(&rows);
        let visible = area.height as usize;
        let top = cursor_vr.saturating_sub(visible.saturating_sub(1));
        let screen_row = row.saturating_sub(area.y) as usize;
        let row_index = (top + screen_row).min(top + visible.saturating_sub(1));
        let Some(vr) = rows.get(row_index) else {
            return rows.last().map_or(0, |row| row.end);
        };

        let target = column.saturating_sub(area.x + Self::prompt_cols()) as usize;
        let mut width = 0;
        for (offset, grapheme) in self.text[vr.start..vr.end].grapheme_indices(true) {
            let start = vr.start + offset;
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if target < width + grapheme_width {
                return start;
            }
            width += grapheme_width;
        }
        vr.end
    }

    fn clear_selection(&mut self) {
        self.anchor = None;
    }

    fn extend_selection(&mut self) {
        self.anchor.get_or_insert(self.cursor);
    }

    fn replace_selection(&mut self, replacement: &str) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };
        let start = range.start;
        self.replace_range(range, replacement);
        self.cursor = start + replacement.len();
        self.clear_selection();
        true
    }

    fn delete_selection(&mut self) -> bool {
        self.replace_selection("")
    }

    fn insert_text(&mut self, text: &str) {
        if !self.replace_selection(text) {
            let start = self.cursor;
            self.replace_range(start..start, text);
            self.cursor = start + text.len();
        }
    }

    fn move_with_selection(&mut self, extend: bool, move_cursor: impl FnOnce(&mut Self)) {
        if extend {
            self.extend_selection();
            move_cursor(self);
            if self.anchor == Some(self.cursor) {
                self.clear_selection();
            }
            return;
        }

        if let Some(range) = self.selection_range() {
            self.cursor = range.start;
            self.clear_selection();
        } else {
            self.clear_selection();
            move_cursor(self);
        }
    }

    fn move_right_with_selection(&mut self, extend: bool) {
        if extend {
            self.extend_selection();
            self.move_right();
            if self.anchor == Some(self.cursor) {
                self.clear_selection();
            }
        } else if let Some(range) = self.selection_range() {
            self.cursor = range.end;
            self.clear_selection();
        } else {
            self.move_right();
        }
    }

    /// The number of *visual* rows the buffer occupies once wrapped to the input
    /// area of `area_width` columns — i.e. how tall the input area must be. Shares
    /// the wrap policy with [`render`](Self::render) via [`visual_rows`](Self::visual_rows),
    /// so sizing and painting can never disagree (the bug that panicked: an area
    /// sized to logical lines while render drew more wrapped rows).
    pub(super) fn line_count(&self, area_width: u16) -> usize {
        self.visual_rows(Self::content_width(area_width))
            .len()
            .max(1)
    }

    /// Columns available for text after the gutter chrome. Centralized so a future
    /// change to the input decoration (e.g. a bordered box instead of `PROMPT`)
    /// is a localized edit here + the per-row paint, not a change to the wrap math.
    fn content_width(area_width: u16) -> usize {
        area_width.saturating_sub(Self::prompt_cols()) as usize
    }

    /// Display width of the left-gutter prompt.
    fn prompt_cols() -> u16 {
        UnicodeWidthStr::width(PROMPT) as u16
    }

    /// Wrap every logical line to `content_width` columns, flattened into visual
    /// rows. Each row carries its logical line and the char range `[start, end)`
    /// of that line it covers; the ranges partition the line with no gaps, which
    /// is what lets [`locate_cursor`](Self::locate_cursor) map a caret to a cell.
    /// Always at least one (possibly empty) row per logical line.
    fn visual_rows(&self, content_width: usize) -> Vec<VisualRow> {
        let mut rows = Vec::new();
        for line_idx in 0..self.line_starts.len() {
            let range = self.line_range(line_idx);
            let text = &self.text[range.clone()];
            if text.is_empty() || content_width == 0 {
                rows.push(VisualRow {
                    line: line_idx,
                    start: range.start,
                    end: range.start,
                });
                continue;
            }
            let mut start = range.start;
            for byte_break in wrap_points(text, content_width) {
                let end = range.start + byte_break;
                rows.push(VisualRow {
                    line: line_idx,
                    start,
                    end,
                });
                start = end;
            }
            rows.push(VisualRow {
                line: line_idx,
                start,
                end: range.end,
            });
        }
        rows
    }

    /// Find the visual row holding the caret and its byte offset within that row.
    /// A caret resting on a wrap boundary belongs to the *next* row's start,
    /// except on a line's final row where it sits just past the final grapheme.
    fn locate_cursor(&self, rows: &[VisualRow]) -> (usize, usize) {
        for (i, vr) in rows.iter().enumerate() {
            if vr.line != self.cursor_row() {
                continue;
            }
            let last_seg = i + 1 >= rows.len() || rows[i + 1].line != vr.line;
            if self.cursor >= vr.start
                && (self.cursor < vr.end || (last_seg && self.cursor == vr.end))
            {
                return (i, self.cursor - vr.start);
            }
        }
        let i = rows.len().saturating_sub(1);
        (i, 0)
    }

    /// The current text.
    pub(super) fn text(&self) -> String {
        self.text.clone()
    }

    /// Reset to an empty single line.
    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.line_starts = vec![0];
        self.cursor = 0;
        self.anchor = None;
        self.goal_col = 0;
    }

    /// Replace the buffer with `text`, cursor at the end — used by the control
    /// strip's stager class to park/restore a draft and pre-fill an argument
    /// (docs/tui.md).
    pub(super) fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.line_starts = std::iter::once(0)
            .chain(
                self.text
                    .bytes()
                    .enumerate()
                    .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
            )
            .collect();
        self.cursor = self.text.len();
        self.anchor = None;
        self.goal_col = self.cursor_col();
    }

    /// Feed one key event. Returns whether it was consumed, ignored (caller should
    /// handle), or a submission.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

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
            KeyCode::Char('a') if ctrl => self.move_with_selection(shift, |this| {
                this.cursor = this.line_range(this.cursor_row()).start;
            }),
            KeyCode::Char('e') if ctrl => self.move_with_selection(shift, |this| {
                this.cursor = this.line_range(this.cursor_row()).end;
            }),
            KeyCode::Char('b') if ctrl => self.move_with_selection(shift, Self::move_left),
            KeyCode::Char('f') if ctrl => self.move_right_with_selection(shift),
            KeyCode::Char('p') if ctrl => {
                if shift {
                    self.extend_selection();
                    if !self.move_up_or_spill() {
                        self.clear_selection();
                        return ComposerAction::SpillUp;
                    }
                } else if let Some(range) = self.selection_range() {
                    self.cursor = range.start;
                    self.clear_selection();
                } else if !self.move_up_or_spill() {
                    return ComposerAction::SpillUp;
                }
            }
            KeyCode::Char('n') if ctrl => self.move_with_selection(shift, Self::move_down),
            KeyCode::Left => self.move_with_selection(shift, Self::move_left),
            KeyCode::Right => self.move_right_with_selection(shift),
            KeyCode::Up => {
                if shift {
                    self.extend_selection();
                    if !self.move_up_or_spill() {
                        self.clear_selection();
                        return ComposerAction::SpillUp;
                    }
                } else if let Some(range) = self.selection_range() {
                    self.cursor = range.start;
                    self.clear_selection();
                } else if !self.move_up_or_spill() {
                    return ComposerAction::SpillUp;
                }
            }
            KeyCode::Down => self.move_with_selection(shift, Self::move_down),
            KeyCode::Home => self.move_with_selection(shift, |this| {
                this.cursor = this.line_range(this.cursor_row()).start;
            }),
            KeyCode::End => self.move_with_selection(shift, |this| {
                this.cursor = this.line_range(this.cursor_row()).end;
            }),
            KeyCode::Char('b') if alt => self.move_with_selection(shift, Self::move_word_left),
            KeyCode::Char('f') if alt => self.move_with_selection(shift, Self::move_word_right),
            KeyCode::Char('h') if ctrl => {
                self.cursor = self.text.len();
                self.anchor = Some(0);
            }

            // Emacs editing.
            KeyCode::Backspace if alt => {
                if !self.delete_selection() {
                    self.delete_word_back();
                }
            }
            KeyCode::Backspace => {
                if !self.delete_selection() {
                    self.delete_back();
                }
            }
            KeyCode::Char('h') if ctrl => {
                if !self.delete_selection() {
                    self.delete_back();
                }
            }
            KeyCode::Delete => {
                if !self.delete_selection() {
                    self.delete_forward();
                }
            }
            KeyCode::Char('d') if ctrl => {
                if !self.delete_selection() {
                    self.delete_forward();
                }
            }
            KeyCode::Char('k') if ctrl => {
                if !self.delete_selection() {
                    self.kill_to_line_end();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if !self.delete_selection() {
                    self.kill_to_line_start();
                }
            }
            KeyCode::Char('w') if ctrl => {
                if !self.delete_selection() {
                    self.delete_word_back();
                }
            }
            KeyCode::Char('d') if alt => {
                if !self.delete_selection() {
                    self.delete_word_forward();
                }
            }

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
            self.goal_col = self.cursor_col();
        }
        ComposerAction::Consumed
    }

    fn insert_char(&mut self, c: char) {
        let mut text = [0; 4];
        self.insert_text(c.encode_utf8(&mut text));
    }

    fn insert_newline(&mut self) {
        self.insert_text("\n");
    }

    fn move_left(&mut self) {
        let row = self.cursor_row();
        let line = self.line_range(row);
        if self.cursor > line.start {
            self.cursor = self.previous_grapheme_boundary(self.cursor);
        } else if row > 0 {
            self.cursor = self.line_range(row - 1).end;
        }
    }

    fn move_right(&mut self) {
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
    fn move_up_or_spill(&mut self) -> bool {
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

    fn move_down(&mut self) {
        let row = self.cursor_row();
        if row + 1 < self.line_starts.len() {
            self.cursor = self.line_offset(row + 1, self.goal_col);
        }
    }

    /// ASCII letters and digits form a word; all other grapheme clusters separate
    /// words.
    fn is_word_grapheme(grapheme: &str) -> bool {
        grapheme.len() == 1 && grapheme.as_bytes()[0].is_ascii_alphanumeric()
    }

    fn move_word_left(&mut self) {
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

    fn move_word_right(&mut self) {
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

    fn delete_back(&mut self) {
        let row = self.cursor_row();
        let line = self.line_range(row);
        if self.cursor > line.start {
            let start = self.previous_grapheme_boundary(self.cursor);
            self.replace_range(start..self.cursor, "");
            self.cursor = start;
        } else if row > 0 {
            let newline = line.start - 1;
            self.replace_range(newline..line.start, "");
            self.cursor = newline;
        }
    }

    fn delete_forward(&mut self) {
        let row = self.cursor_row();
        let line = self.line_range(row);
        if self.cursor < line.end {
            let end = self.next_grapheme_boundary(self.cursor);
            self.replace_range(self.cursor..end, "");
        } else if row + 1 < self.line_starts.len() {
            self.replace_range(line.end..line.end + 1, "");
        }
    }

    fn kill_to_line_end(&mut self) {
        let row = self.cursor_row();
        let line = self.line_range(row);
        if self.cursor < line.end {
            self.replace_range(self.cursor..line.end, "");
        } else if row + 1 < self.line_starts.len() {
            self.replace_range(line.end..line.end + 1, "");
        }
    }

    fn kill_to_line_start(&mut self) {
        let start = self.line_range(self.cursor_row()).start;
        self.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_word_back(&mut self) {
        let end = self.cursor;
        self.move_word_left();
        let start = self.cursor;
        if start < end {
            self.replace_range(start..end, "");
        }
    }

    /// Delete the next word and any leading separators, retaining following separators.
    fn delete_word_forward(&mut self) {
        let start = self.cursor;
        self.move_word_right();
        let end = self.cursor;
        if start < end {
            self.replace_range(start..end, "");
            self.cursor = start;
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
        selection_style: Style,
    ) -> Position {
        if area.width == 0 || area.height == 0 {
            return Position::new(area.x, area.y);
        }
        let prompt_w = Self::prompt_cols();
        let rows = self.visual_rows(Self::content_width(area.width));
        let (cursor_vr, cursor_off) = self.locate_cursor(&rows);

        // Vertical scroll within the input area so the cursor's visual row is shown.
        let visible = area.height as usize;
        let top = cursor_vr.saturating_sub(visible.saturating_sub(1));

        for (screen_row, vr) in rows.iter().enumerate().skip(top).take(visible) {
            let y = area.y + (screen_row - top) as u16;
            let mut x = area.x;
            // Prompt marker on every wrapped row's left gutter.
            for ch in PROMPT.chars() {
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch).set_style(prompt_style);
                }
                x += 1;
            }
            let selection = self.selection_range();
            for (offset, grapheme) in self.text[vr.start..vr.end].grapheme_indices(true) {
                let start = vr.start + offset;
                let end = start + grapheme.len();
                let w = UnicodeWidthStr::width(grapheme) as u16;
                if x + w > area.right() {
                    break;
                }
                let style = if selection
                    .as_ref()
                    .is_some_and(|range| range.start < end && start < range.end)
                {
                    selection_style
                } else {
                    text_style
                };
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(grapheme).set_style(style);
                }
                x += w.max(1);
            }
        }

        // Cursor screen position: prompt + display width up to the caret within
        // its visual row.
        let cursor_screen_row = cursor_vr.saturating_sub(top) as u16;
        let vr = rows[cursor_vr];
        let mut cursor_x = area.x + prompt_w;
        cursor_x += UnicodeWidthStr::width(&self.text[vr.start..vr.start + cursor_off]) as u16;
        Position::new(
            cursor_x.min(area.right().saturating_sub(1)),
            area.y + cursor_screen_row,
        )
    }
}

#[cfg(test)]
mod test;
