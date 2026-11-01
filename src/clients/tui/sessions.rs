//! The session switcher (docs/tui.md): the host's sessions as an overlay, with
//! the two verbs that act on a session's *life* rather than on its loop —
//! switch to it, and archive it.
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
//! rule: a session mid-turn, holding a call at a gate, asking a question, stalled, parked at the cold check, or holding
//! background processes is confirmed; an idle one with nothing running is not.

use super::*;
use crate::protocol::sessions::{ListedSession, ListedStatus};

/// The switcher's state while it is open.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SessionMenu {
    /// The new-session form, while it is open over the listing (docs/tui.md).
    /// It owns every key it is given, including Escape and Ctrl-C.
    pub(super) form: Option<Box<session_form::Form>>,
    /// The resume page, while it is open over the listing (docs/tui.md). Like
    /// the form it owns every key, Escape and Ctrl-C included.
    pub(super) resume: Option<Box<resume_list::ResumePage>>,
    /// The listing as the host last answered it, sorted by id. Empty while the
    /// first read is out.
    pub rows: Vec<ListedSession>,
    /// The row the keys act on. Clamped at use, because the listing is replaced
    /// wholesale and a row can leave under the cursor.
    pub selected: usize,
    /// `Some(id)` while an archive is waiting to be confirmed — the destructive
    /// verb's second press (docs/tui.md).
    pub confirming: Option<String>,
    /// A line under the list: what the switcher is waiting for, what the host
    /// said, or why a confirm is being asked.
    pub note: Option<String>,
}

impl SessionMenu {
    /// The selected row, with the index clamped to the listing as it stands now.
    pub(crate) fn current(&self) -> Option<&ListedSession> {
        self.rows
            .get(self.selected.min(self.rows.len().saturating_sub(1)))
    }
}

/// Why ending this session should be confirmed, or `None` when there is nothing
/// to lose by ending it now.
///
/// The two conditions the listing can answer (`host/listing.rs`): the session is
/// doing something — running a turn, holding a call at a gate, or parked on a
/// stall it has not been told about — or it holds background processes, which
/// ending reaps.
pub(crate) fn needs_confirm(row: &ListedSession) -> Option<String> {
    let doing = match row.status {
        ListedStatus::Running => Some("is mid-turn"),
        ListedStatus::Gated => Some("is holding a call at a gate"),
        ListedStatus::Stalled => Some("stalled and has not been answered"),
        ListedStatus::Asking => Some("is waiting on your answer to a question"),
        ListedStatus::Parked => Some("is paused on an expired cache, waiting for you"),
        ListedStatus::Blocked => Some("is stopped mid-turn until someone acts"),
        // A word this build does not know says nothing to confirm.
        ListedStatus::Starting | ListedStatus::Idle | ListedStatus::Other(_) => None,
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
    pub(super) fn apply_sessions(&mut self, listing: Result<Vec<ListedSession>, String>) {
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

    /// The form's opening read landed: open it over the listing, or say why it
    /// could not be opened. Ignored when the switcher has been closed since `n`
    /// was pressed — the form is overlay state and has nowhere else to live.
    pub(super) fn open_new_session_form(
        &mut self,
        context: Result<session_form::NewSessionContext, String>,
    ) {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return;
        };
        match context {
            Ok(context) => {
                let mut form = Box::new(session_form::Form::new(context));
                menu.note = None;
                // Resolve the empty draft before the operator touches anything:
                // the defaults are most of what the form shows, and they are the
                // host's answer rather than something the client can assume
                // (docs/control-api.md).
                if let session_form::Action::Preview(options) = form.initial_preview() {
                    let _ = self.host_tx.send(HostCmd::Preview(options));
                }
                menu.form = Some(form);
            }
            Err(error) => menu.note = Some(error),
        }
    }

    /// A preview answered. Dropped when no form is open: a reply that outlived
    /// its form describes a draft nobody can see.
    pub(super) fn apply_preview(
        &mut self,
        resolved: Result<crate::inference::ResolvedOptions, String>,
    ) {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return;
        };
        let Some(form) = &mut menu.form else {
            return;
        };
        // A superseded reply asks for the current draft to be resolved instead.
        if let session_form::Action::Preview(options) = form.preview_returned(resolved) {
            let _ = self.host_tx.send(HostCmd::Preview(options));
        }
    }

    /// One connection's catalog entry answered. Compacted into the open form's
    /// picker; dropped when no form is open, since the entry describes a list
    /// nobody can see.
    pub(super) fn apply_catalog(
        &mut self,
        connection: &str,
        result: Result<crate::clients::client::ConnectionCatalog, String>,
    ) {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return;
        };
        if let Some(form) = &mut menu.form {
            form.catalog_returned(connection, result);
        }
    }

    /// Creation answered. On success the TUI leaves this session's stream for the
    /// new one, which is the same one-cursor-per-handshake switch `⏎` performs —
    /// the session was created in this client's own working root, so that is
    /// where its attachment store lives (docs/inbox.md).
    pub(super) fn apply_created(&mut self, created: Result<String, String>) -> bool {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return false;
        };
        match created {
            Ok(id) => {
                self.switch_to = Some(self.switch_exit(id));
                true
            }
            Err(error) => {
                if let Some(form) = &mut menu.form {
                    form.creation_failed(error);
                } else {
                    menu.note = Some(error);
                }
                false
            }
        }
    }

    /// The resume page's rows landed (or the read failed). Dropped when the page
    /// has been closed since `r` was pressed.
    pub(super) fn apply_archived(&mut self, rows: Result<Vec<resume_list::ResumeRow>, String>) {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return;
        };
        let Some(page) = &mut menu.resume else {
            return;
        };
        match rows {
            Ok(rows) => page.set_rows(rows),
            Err(error) => page.read_failed(error),
        }
    }

    /// A revive answered. On success the TUI leaves this session's stream for the
    /// revived one, as `⏎` on a live row does; on failure the page stays, with
    /// the host's message. Dropped when the page was closed meanwhile.
    pub(super) fn apply_resumed(&mut self, resumed: Result<String, String>) -> bool {
        let page_open = matches!(&self.ui_mode, UiMode::Sessions(menu) if menu.resume.is_some());
        // The retry of a failed resume, from its own view (`starting.rs`).
        if !page_open && self.start_failed() {
            return match resumed {
                Ok(id) => {
                    self.switch_to = Some(self.switch_exit(id));
                    true
                }
                Err(error) => {
                    self.ui.debug_message = error.into();
                    false
                }
            };
        }
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return false;
        };
        let Some(page) = &mut menu.resume else {
            return false;
        };
        match resumed {
            Ok(id) => {
                self.switch_to = Some(Exit::Switch { id });
                true
            }
            Err(error) => {
                page.resume_failed(error);
                false
            }
        }
    }

    /// A key while the resume page is open. Esc on an empty query and Ctrl-C
    /// close it back to the listing; Enter asks the host to revive the row.
    pub(super) fn handle_resume_key(&mut self, key: event::KeyEvent) {
        use super::resume_list::ResumeKey;
        use crossterm::event::{KeyCode, KeyModifiers};
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return;
        };
        let Some(page) = &mut menu.resume else {
            return;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            menu.resume = None;
            return;
        }
        match page.handle(key) {
            ResumeKey::Continue => {}
            ResumeKey::Back => menu.resume = None,
            ResumeKey::Pick(id) => {
                page.begin_resume(&id);
                let _ = self.host_tx.send(HostCmd::Resume(id));
            }
        }
    }

    /// The switcher's keys. Returns whether the UI loop should end — which a
    /// switch does, with [`switch_to`](Self::switch_to) naming where to go next.
    pub(super) fn handle_sessions_key(&mut self, key: event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return false;
        };
        // `n` opens the new-session form. It paints immediately against an empty
        // context and a note; the working root and the connection inventory land
        // as a [`TuiEvent::NewSession`].
        if key.code == KeyCode::Char('n') {
            menu.note = Some("reading the host's connections…".into());
            let _ = self.host_tx.send(HostCmd::NewSession);
            return false;
        }
        // While a confirm is up it owns every key: `y` (or ⏎) goes through and
        // anything else is a cancel, so a mistyped key can only ever back out.
        if let Some(id) = menu.confirming.clone() {
            menu.confirming = None;
            if matches!(key.code, KeyCode::Enter | KeyCode::Char('y' | 'Y')) {
                menu.note = Some(format!("archiving {id}…"));
                let _ = self.host_tx.send(HostCmd::Archive(id));
            } else {
                menu.note = None;
            }
            return false;
        }
        // `r` opens the resume page over the listing: archived sessions here,
        // read from the host. It paints at once with a "reading…" line.
        if key.code == KeyCode::Char('r') {
            menu.resume = Some(Box::new(resume_list::ResumePage::loading()));
            let _ = self.host_tx.send(HostCmd::Archived);
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
            KeyCode::Char('a') | KeyCode::Delete => return self.archive_selected_session(),
            KeyCode::Enter => return self.switch_to_selected_session(),
            _ => {}
        }
        false
    }

    /// `a` on a row: archive it now, or raise the confirm when there is something to
    /// lose (see [`needs_confirm`]).
    fn archive_selected_session(&mut self) -> bool {
        let UiMode::Sessions(menu) = &mut self.ui_mode else {
            return false;
        };
        let Some(row) = menu.current().cloned() else {
            return false;
        };
        match needs_confirm(&row) {
            Some(reason) => {
                menu.note = Some(format!("{reason}. archive it anyway? y / any other key"));
                menu.confirming = Some(row.id);
            }
            None => {
                menu.note = Some(format!("archiving {}…", row.id));
                let _ = self.host_tx.send(HostCmd::Archive(row.id));
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
        if Some(&row.id) == self.topics.session_identity.as_ref().map(|i| &i.id) {
            self.ui_mode = UiMode::Normal;
            self.composing = !self.read_only();
            return false;
        }
        self.switch_to = Some(Exit::Switch { id: row.id });
        true
    }
}

#[cfg(test)]
mod test;
