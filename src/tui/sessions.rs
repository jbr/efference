//! The session switcher (docs/tui.md): the host's sessions as an overlay, with
//! the two verbs that act on a session's *life* rather than on its loop —
//! switch to it, and end it.
//!
//! The TUI streams exactly one session. Switching is therefore not a second
//! subscription: it closes this session's stream and opens the next one, which
//! is what the per-session one-cursor-per-handshake contract requires
//! (docs/control-api.md). The loop returns [`Exit::Switch`] and the entry point
//! re-attaches; nothing here tries to hold two streams open.
//!
//! Ending is destructive and irreversible from the client's side — the host
//! reaps the session's background processes and removes its scratch — so it
//! asks first when there is something to lose. [`needs_confirm`] is the whole
//! rule: a session mid-turn, holding a call at a gate, stalled, or holding
//! background processes is confirmed; an idle one with nothing running is not.

use super::*;
use crate::client::SessionSummary;

/// The switcher's state while it is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionMenu {
    #[cfg(feature = "dev")]
    pub prototype: bool,
    #[cfg(feature = "dev")]
    pub(super) form: Option<Box<super::session_form::Form>>,
    /// The listing as the host last answered it, sorted by id. Empty while the
    /// first read is out.
    pub rows: Vec<SessionSummary>,
    /// The row the keys act on. Clamped at use, because the listing is replaced
    /// wholesale and a row can leave under the cursor.
    pub selected: usize,
    /// `Some(id)` while an end is waiting to be confirmed — the destructive
    /// verb's second press (docs/tui.md).
    pub confirming: Option<String>,
    /// A line under the list: what the switcher is waiting for, what the host
    /// said, or why a confirm is being asked.
    pub note: Option<String>,
}

impl SessionMenu {
    /// The selected row, with the index clamped to the listing as it stands now.
    pub(crate) fn current(&self) -> Option<&SessionSummary> {
        self.rows
            .get(self.selected.min(self.rows.len().saturating_sub(1)))
    }
}

/// Why ending this session should be confirmed, or `None` when there is nothing
/// to lose by ending it now.
///
/// The two conditions the listing can answer (`host/summary.rs`): the session is
/// doing something — running a turn, holding a call at a gate, or parked on a
/// stall it has not been told about — or it holds background processes, which
/// ending reaps.
pub(crate) fn needs_confirm(row: &SessionSummary) -> Option<String> {
    let doing = match row.status.as_str() {
        "running" => Some("is mid-turn"),
        "gated" => Some("is holding a call at a gate"),
        "stalled" => Some("stalled and has not been answered"),
        _ => None,
    };
    match (doing, row.processes) {
        (None, 0) => None,
        (Some(doing), 0) => Some(format!("{} {doing}", row.id)),
        (None, n) => Some(format!(
            "{} holds {n} background process{}",
            row.id,
            plural(n)
        )),
        (Some(doing), n) => Some(format!(
            "{} {doing} and holds {n} background process{}",
            row.id,
            plural(n)
        )),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "es" }
}

impl InteractiveState<'_> {
    /// Open the switcher and ask the host for its listing. The overlay paints
    /// immediately with an empty list and a note; the rows land as a
    /// [`TuiEvent::Sessions`].
    pub(super) fn open_sessions(&mut self) {
        self.ui_mode = UiMode::Sessions(SessionMenu {
            note: Some("reading the host's sessions…".into()),
            ..Default::default()
        });
        let _ = self.host_tx.send(HostCmd::List);
    }

    /// Fold a listing (or the reason there isn't one) into an open switcher.
    /// Ignored when the switcher is closed: the listing is overlay state, and
    /// nothing else in the UI reads it.
    pub(super) fn apply_sessions(&mut self, listing: Result<Vec<SessionSummary>, String>) {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return;
        };
        match listing {
            Ok(rows) => {
                // The cursor follows the id it was on rather than the position,
                // so a row leaving does not silently re-aim a destructive verb
                // at whatever slid into its place.
                let held = menu.current().map(|row| row.id.clone());
                menu.selected = held
                    .and_then(|id| rows.iter().position(|row| row.id == id))
                    .unwrap_or(0);
                menu.note = rows
                    .is_empty()
                    .then(|| "this host holds no sessions.".to_string());
                // A confirm whose row has gone has nothing left to confirm.
                if let Some(id) = &menu.confirming
                    && !rows.iter().any(|row| &row.id == id)
                {
                    menu.confirming = None;
                }
                menu.rows = rows;
            }
            Err(error) => {
                menu.rows.clear();
                menu.confirming = None;
                menu.note = Some(error);
            }
        }
    }

    /// The switcher's keys. Returns whether the UI loop should end — which a
    /// switch does, with [`switch_to`](Self::switch_to) naming where to go next.
    pub(super) fn handle_sessions_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return false;
        };
        #[cfg(feature = "dev")]
        if menu.prototype && key.code == KeyCode::Char('n') {
            menu.form = Some(Box::default());
            return false;
        }
        // While a confirm is up it owns every key: `y` (or ⏎) goes through and
        // anything else is a cancel, so a mistyped key can only ever back out.
        if let Some(id) = menu.confirming.clone() {
            menu.confirming = None;
            if matches!(key.code, KeyCode::Enter | KeyCode::Char('y' | 'Y')) {
                menu.note = Some(format!("ending {id}…"));
                let _ = self.host_tx.send(HostCmd::End(id));
            } else {
                menu.note = None;
            }
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                menu.selected = menu.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = menu.rows.len().saturating_sub(1);
                menu.selected = (menu.selected + 1).min(last);
            }
            KeyCode::Char('e') | KeyCode::Delete => return self.end_selected_session(),
            KeyCode::Enter => return self.switch_to_selected_session(),
            _ => {}
        }
        false
    }

    /// `e` on a row: end it now, or raise the confirm when there is something to
    /// lose (see [`needs_confirm`]).
    fn end_selected_session(&mut self) -> bool {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return false;
        };
        let Some(row) = menu.current().cloned() else {
            return false;
        };
        match needs_confirm(&row) {
            Some(reason) => {
                menu.note = Some(format!("{reason}. end it anyway? y / any other key"));
                menu.confirming = Some(row.id);
            }
            None => {
                menu.note = Some(format!("ending {}…", row.id));
                let _ = self.host_tx.send(HostCmd::End(row.id));
            }
        }
        false
    }

    /// `⏎` on a row: leave this session's stream for that one. The row already
    /// selected is a no-op that closes the overlay, since re-attaching to the
    /// session already on screen would only cost it its scrollback.
    fn switch_to_selected_session(&mut self) -> bool {
        let UiMode::Sessions(menu) = &self.ui_mode else {
            return false;
        };
        let Some(row) = menu.current().cloned() else {
            return false;
        };
        if Some(&row.id) == self.session_identity.as_ref().map(|i| &i.id) {
            self.ui_mode = UiMode::Normal;
            self.composing = !self.read_only();
            return false;
        }
        self.switch_to = Some(Exit::Switch {
            id: row.id,
            cwd: row.cwd,
        });
        true
    }
}

#[cfg(test)]
mod test;
