//! The composer's undo and redo history, with typing coalesced into runs.

use super::*;

impl Composer {
    pub(super) fn state(&self) -> ComposerState {
        ComposerState {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
            goal_col: self.goal_col,
        }
    }

    pub(super) fn restore_state(&mut self, state: ComposerState) {
        self.text = state.text;
        self.line_starts = std::iter::once(0)
            .chain(
                self.text
                    .bytes()
                    .enumerate()
                    .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
            )
            .collect();
        self.cursor = state.cursor;
        self.anchor = state.anchor;
        self.goal_col = state.goal_col;
    }

    pub(super) fn reset_history(&mut self) {
        self.history.clear();
        self.history_position = 0;
        self.coalescing_run = CoalescingRun::None;
    }

    /// Record one edit, optionally extending the immediately preceding run.
    pub(super) fn record_edit(&mut self, before: ComposerState, run: CoalescingRun) {
        let after = self.state();
        if before == after {
            self.coalescing_run = CoalescingRun::None;
            return;
        }

        let extends_previous = match (self.coalescing_run, run) {
            (
                CoalescingRun::Typing {
                    last_inserted_at,
                    end_cursor,
                },
                CoalescingRun::Typing {
                    last_inserted_at: now,
                    ..
                },
            ) => {
                self.history_position == self.history.len()
                    && self
                        .history
                        .last()
                        .is_some_and(|entry| entry.after.cursor == end_cursor)
                    && now.duration_since(last_inserted_at) <= TYPING_BURST_IDLE
            }
            (CoalescingRun::Backspace, CoalescingRun::Backspace)
            | (CoalescingRun::DeleteForward, CoalescingRun::DeleteForward) => {
                self.history_position == self.history.len()
            }

            _ => false,
        };

        if extends_previous {
            self.history.last_mut().unwrap().after = after;
        } else {
            self.history.truncate(self.history_position);
            self.history.push(HistoryEntry { before, after });

            if self.history.len() > HISTORY_LIMIT {
                self.history.remove(0);
            }
            self.history_position = self.history.len();
        }

        self.coalescing_run = run;
    }

    pub(super) fn edit(&mut self, run: CoalescingRun, edit: impl FnOnce(&mut Self)) {
        let before = self.state();
        edit(self);
        self.record_edit(before, run);
    }

    pub(super) fn break_coalescing(&mut self) {
        self.coalescing_run = CoalescingRun::None;
    }

    /// Restore the state before the previous text-changing operation.
    pub(in crate::tui) fn undo(&mut self) {
        let Some(position) = self.history_position.checked_sub(1) else {
            return;
        };
        let entry = self.history[position].clone();
        self.restore_state(entry.before);
        self.history_position = position;
        self.break_coalescing();
    }

    /// Restore the state after the next text-changing operation.
    pub(in crate::tui) fn redo(&mut self) {
        let Some(entry) = self.history.get(self.history_position).cloned() else {
            return;
        };
        self.restore_state(entry.after);
        self.history_position += 1;
        self.break_coalescing();
    }
}
