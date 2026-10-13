//! The input composer — a minimal, dependency-free multiline text editor with
//! emacs-style key bindings (docs/tui.md).
//!
//! Self-contained behind a small interface ([`handle_key`](Composer::handle_key) /
//! [`render`](Composer::render) / [`text`](Composer::text)), so it can be replaced
//! wholesale without touching the rest of the TUI. `tui-textarea` is pinned to the
//! older ratatui/unicode-width ecosystem and does not resolve against ratatui 0.30 —
//! hence this hand-rolled editor.
//!
//! The buffer is one UTF-8 string. Cursor and layout positions are byte offsets
//! constrained to extended-grapheme boundaries, so edits cannot split a
//! user-perceived character.

use std::{
    ops::Range,
    time::{Duration, Instant},
};

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

/// Maximum number of text changes retained for undo and redo.
const HISTORY_LIMIT: usize = 200;

/// Idle time after which ordinary typing begins a new undoable burst.
const TYPING_BURST_IDLE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerState {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    goal_col: usize,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    before: ComposerState,
    after: ComposerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoalescingRun {
    None,
    Typing {
        last_inserted_at: Instant,
        end_cursor: usize,
    },
    Backspace,
    DeleteForward,
}

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
    /// Text-changing operations before and after the current history position.
    history: Vec<HistoryEntry>,
    /// Number of entries from `history` currently applied to the buffer.
    history_position: usize,
    /// The edit kind eligible to extend the latest history entry.
    coalescing_run: CoalescingRun,
}

impl Composer {
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            line_starts: vec![0],
            cursor: 0,
            anchor: None,
            goal_col: 0,
            history: Vec::new(),
            history_position: 0,
            coalescing_run: CoalescingRun::None,
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

    /// The caret's byte offset in [`text`](Self::text), on a grapheme boundary.
    pub(super) fn cursor(&self) -> usize {
        self.cursor
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
        self.reset_history();
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
        self.reset_history();
    }

    /// Feed one key event. Returns whether it was consumed, ignored (caller should
    /// handle), or a submission.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ordinary_insertion = matches!(key.code, KeyCode::Char(_))
            && !ctrl
            && !alt
            && !shift
            && self.selection_range().is_none();
        let keeps_coalescing = ordinary_insertion
            || matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
            || (ctrl && matches!(key.code, KeyCode::Char('d')));

        match key.code {
            // Undo stays on the composer so the non-Super terminal bindings work
            // without keyboard enhancement support.
            KeyCode::Char('/') if ctrl => self.undo(),
            KeyCode::Char('_') if ctrl => self.undo(),
            KeyCode::Char('_') if alt => self.redo(),

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
            KeyCode::Tab => self.insert_tab(),

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
                if !self.delete_selection_as_edit() {
                    self.delete_word_back();
                }
            }
            KeyCode::Backspace => {
                if !self.delete_selection_as_edit() {
                    self.delete_back();
                }
            }
            KeyCode::Delete => {
                if !self.delete_selection_as_edit() {
                    self.delete_forward();
                }
            }
            KeyCode::Char('d') if ctrl => {
                if !self.delete_selection_as_edit() {
                    self.delete_forward();
                }
            }
            KeyCode::Char('k') if ctrl => {
                if !self.delete_selection_as_edit() {
                    self.kill_to_line_end();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if !self.delete_selection_as_edit() {
                    self.kill_to_line_start();
                }
            }
            KeyCode::Char('w') if ctrl => {
                if !self.delete_selection_as_edit() {
                    self.delete_word_back();
                }
            }
            KeyCode::Char('d') if alt => {
                if !self.delete_selection_as_edit() {
                    self.delete_word_forward();
                }
            }

            // Printable insertion (ignore other control combos so they fall
            // through to the caller as commands).
            KeyCode::Char(c) if !ctrl && !alt => self.insert_char(c),

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
        if !keeps_coalescing {
            self.break_coalescing();
        }
        ComposerAction::Consumed
    }
}

#[cfg(test)]
mod test;

mod editing;
mod history;
mod layout;
mod selection;
