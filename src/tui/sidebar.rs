//! Sidebar focus (docs/tui.md): Tab toggles the right-docked sidebar's *actionable*
//! rows into the focus model, so its rows can carry verbs instead of being read-only
//! chrome. The third context on top of the vertical axis (transcript → strip →
//! composer): mutually exclusive with `composing` and with `strip_focus`.
//!
//! Rows are held by identity ([`SidebarRow`]) — a process id or a proposal's `seq` —
//! never by screen position. Every activation re-resolves the identity against the
//! current data before firing, so a list that reordered or shrank between focus
//! landing and `⏎` can only ever no-op with a note, never substitute a different
//! row's verb. Verbs follow the block menu's grammar one layer over: `←`/`→` choose,
//! `⏎` fires, the selection resets to the leading verb on a row change.
//!
//! Like the strip, this is chrome-side state: nothing here is projected or logged.

use std::io::Write;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::prelude::Backend;

use super::{InteractiveState, SidebarCmd, SidebarReturn, SidebarRow};

/// One verb a focused row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SidebarVerb {
    Kill,
    Commit,
    Cancel,
}

impl SidebarVerb {
    /// The button label, matching the strip's and block menu's lowercase style.
    pub(super) fn label(self) -> &'static str {
        match self {
            SidebarVerb::Kill => "kill",
            SidebarVerb::Commit => "commit",
            SidebarVerb::Cancel => "cancel",
        }
    }
}

impl<'a> InteractiveState<'a> {
    /// The sidebar's actionable rows in render order: every live process
    /// top-to-bottom, then POLICY's proposed entries (scope first, then
    /// commands/paths/tools, each in its committed sort position). The single source
    /// the renderer walks and the dispatcher resolves against, so geometry and
    /// dispatch cannot drift apart.
    pub(in crate::tui) fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut rows: Vec<SidebarRow> = self
            .processes
            .iter()
            .map(|p| SidebarRow::Process(p.id))
            .collect();
        if let Some(policy) = &self.policy {
            if let Some((seq, _)) = policy.scope_proposal {
                rows.push(SidebarRow::Proposal(seq));
            }
            for section in [&policy.commands, &policy.paths, &policy.tools] {
                for entry in section {
                    if let (true, Some(seq)) = (entry.proposed, entry.seq) {
                        rows.push(SidebarRow::Proposal(seq));
                    }
                }
            }
        }
        rows
    }

    /// The verbs of one row. A process kills (the daemon answers a stale id 404);
    /// a proposal commits or cancels (`PATCH /proposals/:seq`, docs/permissions.md).
    pub(super) fn sidebar_verbs(&self, row: SidebarRow) -> Vec<SidebarVerb> {
        match row {
            SidebarRow::Process(_) => vec![SidebarVerb::Kill],
            SidebarRow::Proposal(_) => vec![SidebarVerb::Commit, SidebarVerb::Cancel],
        }
    }

    /// Resolve a row's identity against the current data — its position among the
    /// actionable rows right now, or `None` if it has vanished (process exited,
    /// proposal resolved elsewhere).
    pub(super) fn resolve_sidebar_row(&self, row: SidebarRow) -> Option<usize> {
        self.sidebar_rows().iter().position(|r| *r == row)
    }

    /// Tab from command mode: into the sidebar, or back out of it to where focus
    /// came from. The branch reads `sidebar_open`, not the selected row — an open
    /// column with no row selected (what a vanished row leaves behind) is *in* the
    /// sidebar, and Tab leaves it rather than re-entering.
    pub(super) fn toggle_sidebar(&mut self) {
        if self.sidebar_open {
            self.leave_sidebar();
        } else {
            self.enter_sidebar();
        }
    }

    /// Enter the sidebar, landing on its first actionable row. A read-only view has
    /// no sidebar at all (every section is fed by live state topics), and an empty
    /// sidebar has nothing to act on — both are status-line no-ops rather than a
    /// mode you can enter but not use.
    pub(super) fn enter_sidebar(&mut self) {
        if self.read_only() {
            self.ui.debug_message = "no sidebar in a read-only view".into();
            return;
        }
        let rows = self.sidebar_rows();
        if rows.is_empty() {
            self.ui.debug_message = "nothing actionable in the sidebar".into();
            return;
        }
        // Remember where focus came from so Tab back is exact. The composer is not
        // a source — Tab types there — so the alternatives are strip and transcript.
        self.sidebar_return = Some(match self.strip_focus.take() {
            Some(i) => SidebarReturn::Strip(i),
            None => SidebarReturn::Transcript,
        });
        self.sidebar_open = true;
        self.sidebar_focus = rows.first().copied();
        self.sidebar_verb_focus = 0;
        self.ui.debug_message =
            "sidebar — ↑/↓ row · ←/→ verb · ⏎ act · tab back · esc compose".into();
    }

    /// Leave the sidebar for wherever focus entered from. The transcript cursor was
    /// never touched while the sidebar held focus, so returning there restores the
    /// exact position; the strip restores to its remembered item.
    pub(super) fn leave_sidebar(&mut self) {
        match self.sidebar_return.take() {
            Some(SidebarReturn::Strip(i)) => {
                self.close_sidebar();
                self.strip_focus = Some(i);
                self.ui.debug_message =
                    "controls — ←/→ item · ⏎ act · ↑ navigate · ↓ compose".into();
            }
            Some(SidebarReturn::Transcript) | None => {
                self.close_sidebar();
                self.composing = false;
                self.strip_focus = None;
                self.ui.debug_message =
                    "navigate — ↑/↓ unit · ←/→ verb · ⏎ act · esc compose".into();
            }
        }
    }

    /// Clear the sidebar context (selection with it). Esc's home rung uses this too.
    pub(in crate::tui) fn close_sidebar(&mut self) {
        self.sidebar_open = false;
        self.sidebar_focus = None;
        self.sidebar_verb_focus = 0;
    }

    /// Re-resolve the selected row against current data. Called on entry to sidebar
    /// key handling: a row that vanished since the last frame (process exited,
    /// proposal committed elsewhere) clears the *selection* but not the column — an
    /// auto-reselect here would point the very next `⏎` at whatever occupies that
    /// position now, which is exactly the substitution identity forbids.
    fn refresh_sidebar_focus(&mut self) {
        if let Some(focus) = self.sidebar_focus
            && self.resolve_sidebar_row(focus).is_none()
        {
            self.sidebar_focus = None;
            self.sidebar_verb_focus = 0;
        }
    }

    /// Keys while the sidebar holds focus (docs/tui.md): `↑`/`↓` move between
    /// actionable rows (clamped — the column is self-contained; Tab and Esc are the
    /// exits; with no row selected they pick from either end), `←`/`→` choose among
    /// the selected row's verbs, `⏎`/space fires the selected verb — and no-ops
    /// when no row is selected, which is the state a vanished row leaves behind.
    /// Anything else falls through to the command keys (scroll, help, quit), as in
    /// the strip.
    pub(super) fn handle_sidebar_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        self.refresh_sidebar_focus();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => self.move_sidebar_row(false),
            KeyCode::Char('p') if ctrl => self.move_sidebar_row(false),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => self.move_sidebar_row(true),
            KeyCode::Char('n') if ctrl => self.move_sidebar_row(true),
            KeyCode::Left => self.move_sidebar_verb(false),
            KeyCode::Char('b') if ctrl => self.move_sidebar_verb(false),
            KeyCode::Right => self.move_sidebar_verb(true),
            KeyCode::Char('f') if ctrl => self.move_sidebar_verb(true),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_sidebar_row(),
            KeyCode::Tab => self.leave_sidebar(),
            _ => return self.handle_command_key(key, terminal),
        }
        false
    }

    /// Move between actionable rows. Movement clamps at both ends: the sidebar is a
    /// separate column, not a stop on the vertical axis, and spilling past its last
    /// row would land focus on whichever transcript unit happens to be adjacent.
    fn move_sidebar_row(&mut self, down: bool) {
        let rows = self.sidebar_rows();
        if rows.is_empty() {
            self.ui.debug_message = "nothing actionable in the sidebar".into();
            return;
        }
        let next = match self
            .sidebar_focus
            .and_then(|f| rows.iter().position(|r| *r == f))
        {
            Some(i) => {
                if down {
                    (i + 1).min(rows.len() - 1)
                } else {
                    i.saturating_sub(1)
                }
            }
            None => {
                if down {
                    0
                } else {
                    rows.len() - 1
                }
            }
        };
        self.sidebar_focus = Some(rows[next]);
        self.sidebar_verb_focus = 0;
    }

    /// Choose among the focused row's verbs (`←`/`→`), clamped like the block menu.
    fn move_sidebar_verb(&mut self, right: bool) {
        let Some(focus) = self.sidebar_focus else {
            return;
        };
        let count = self.sidebar_verbs(focus).len();
        if right {
            self.sidebar_verb_focus = (self.sidebar_verb_focus + 1).min(count - 1);
        } else {
            self.sidebar_verb_focus = self.sidebar_verb_focus.saturating_sub(1);
        }
    }

    /// Fire the focused row's selected verb. The identity is resolved against
    /// current data first: a vanished row no-ops with a note (the daemon would
    /// answer 404 anyway), never fires whatever occupies its old position.
    pub(super) fn activate_sidebar_row(&mut self) {
        let Some(focus) = self.sidebar_focus else {
            return;
        };
        if self.resolve_sidebar_row(focus).is_none() {
            self.ui.debug_message = match focus {
                SidebarRow::Process(id) => format!("p#{id} already exited").into(),
                SidebarRow::Proposal(seq) => format!("proposal {seq} already resolved").into(),
            };
            self.refresh_sidebar_focus();
            return;
        }
        let verbs = self.sidebar_verbs(focus);
        let idx = self.sidebar_verb_focus.min(verbs.len() - 1);
        let cmd = match (focus, verbs[idx]) {
            (SidebarRow::Process(id), SidebarVerb::Kill) => {
                self.ui.debug_message = format!("killing p#{id}…").into();
                SidebarCmd::KillProcess(id)
            }
            (SidebarRow::Proposal(seq), SidebarVerb::Commit) => {
                self.ui.debug_message = format!("committing proposal {seq}…").into();
                SidebarCmd::CommitProposal(seq)
            }
            (SidebarRow::Proposal(seq), SidebarVerb::Cancel) => {
                self.ui.debug_message = format!("cancelling proposal {seq}…").into();
                SidebarCmd::CancelProposal(seq)
            }
            _ => return,
        };
        let _ = self.sidebar_tx.send(cmd);
    }
}
