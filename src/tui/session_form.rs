//! The new-session form (docs/tui.md): the client half of the session-options
//! surface `QUERY /sessions` and `POST /sessions` serve (docs/control-api.md).
//!
//! Four host-owned inputs meet here and nowhere else — the connection inventory
//! from `GET /inference`, the recent main picks across all connections from
//! `GET /inference/recent-picks` (offered in the main picker's menu only),
//! the effective draft `QUERY /sessions` recomputes after each edit, and the
//! creation the form finally posts. The form owns none of
//! them: it holds the operator's raw overrides ([`Draft`]) and the host's latest
//! answer about them ([`ResolvedOptions`]) side by side, and renders the
//! difference.
//!
//! Preview is recomputed on *committed* edits — leaving a picker or accepting an
//! editor — never per keystroke. Typing inside the model filter and the text
//! editors is local, so the request rate follows selections rather than
//! characters and no debounce timer is needed.

mod draft;
mod inline;
mod line;
mod pages;
pub(super) mod render;
#[cfg(test)]
mod test;

pub use inline::{run, run_hosted};

pub(crate) use draft::Role;
pub(in crate::tui) use draft::{Draft, RoleDraft, TriageDraft};

use crate::inference::{ConnectionInventory, ResolvedOptions, SessionOptions};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use line::Line;
use std::path::PathBuf;

/// The form page's fixed rows: start, main, effort, temperature, spotter,
/// triage. Named because the height calculation and the cursor bound are the
/// same six (`render.rs`).
const FORM_ROWS: usize = 6;

const EFFORTS: &[&str] = &["Use default", "low", "medium", "high", "xhigh", "max"];

/// Where a created session would run, and what the host has to offer for it.
/// The client that opened the form supplies both: the working root is that
/// client's own, and so is the environment creation will carry — which is why
/// the form displays this rather than asking, and why it cannot be edited until
/// there is a way to capture an environment at an arbitrary directory
/// (backlog/new-session-form.md).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NewSessionContext {
    pub(crate) cwd: PathBuf,
    /// `GET /inference`, sorted by name. Empty until the first read lands, which
    /// the form states rather than rendering as an empty menu.
    pub(crate) connections: Vec<ConnectionInventory>,
    /// `GET /inference/recent-picks` for the same root: past picks across all
    /// connections, most decayed weight first. Empty when nothing was ever
    /// picked — the picker then offers only the default and the connections.
    pub(crate) recent_picks: Vec<crate::client::RecentPick>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Form,
    Connections {
        role: Role,
        selected: usize,
    },
    /// Recent models on one connection, most decayed weight first, with the
    /// typed-ID editor as the last row. Entered with whatever the host has
    /// answered so far: an empty list is a picker with only the typed path,
    /// not a failure — which is also what a no-host form always shows.
    Models {
        role: Role,
        connection: usize,
        models: Vec<crate::client::ModelSuggestion>,
        selected: usize,
    },
    Text {
        role: Role,
        connection: usize,
        value: Line,
    },
    Effort {
        selected: usize,
    },
    Temperature {
        value: Line,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Form {
    context: NewSessionContext,
    draft: Draft,
    /// The host's last answer about [`draft`](Self::draft). `None` before the
    /// first preview lands; stale between an edit and its reply, which the
    /// rendering marks rather than blanking — a blank form on every keystroke
    /// reads worse than a value one edit behind.
    resolved: Option<ResolvedOptions>,
    /// A preview is out for a draft that has since changed again. The reply to
    /// the earlier request describes a draft nobody is looking at, so it is
    /// dropped and the current draft is re-sent (see [`Form::preview`]).
    pending: Option<SessionOptions>,
    selected: usize,
    page: Page,
    /// True from the moment creation is posted until the host answers. Creation
    /// is not idempotent, so the accept key is refused while one is in flight.
    creating: bool,
    note: Option<String>,
}

/// The connection a recents menu was opened on: the role being picked, and
/// the connection's index in the inventory. The container resolves the name
/// and the working root, which never enter the UI thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecentsRequest {
    pub(crate) role: Role,
    pub(crate) connection: usize,
}

/// What a keypress asked the container to do. The form issues no requests
/// itself: it names them, and the container's host dispatcher addresses the
/// socket (`tui/run.rs`).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    Continue,
    Cancel,
    /// Recompute the effective draft — a committed edit, not a keystroke.
    Preview(SessionOptions),
    /// Post the creation. The container attaches the working root and the
    /// environment, which never enter the UI thread (docs/env.md).
    Create(SessionOptions),
    /// Read this connection's recent models for the open menu. Answered with
    /// whatever the host has; an empty answer is a typed-ID-only menu.
    Recents(RecentsRequest),
}

impl Form {
    pub(crate) fn new(context: NewSessionContext) -> Self {
        Self {
            context,
            draft: Draft::default(),
            resolved: None,
            pending: None,
            selected: 0,
            page: Page::Form,
            creating: false,
            note: None,
        }
    }

    /// Where a session created from this form would run — the root the client
    /// supplied, and the one creation sends.
    pub(crate) fn cwd(&self) -> &std::path::Path {
        &self.context.cwd
    }

    /// Resolve the untouched draft, so the form opens showing the defaults a
    /// session would actually get rather than blanks. Same request as any later
    /// edit; it is named separately only because nothing was edited to trigger it.
    pub(crate) fn initial_preview(&mut self) -> Action {
        self.preview()
    }

    /// The inventory landed (or was replaced by a re-read). Selections already
    /// made are kept even if their connection has gone: the host is the
    /// authority on whether a draft is usable, and it reports that as an issue
    /// rather than the client silently discarding an edit.
    pub(crate) fn connections_loaded(&mut self, connections: Vec<ConnectionInventory>) {
        self.context.connections = connections;
    }

    /// Fold in a preview reply. Dropped when the draft has moved on since the
    /// request went out — the answer describes a draft that no longer exists.
    pub(crate) fn preview_returned(&mut self, resolved: Result<ResolvedOptions, String>) -> Action {
        if let Some(superseded) = self.pending.take()
            && superseded != self.draft.to_options()
        {
            return self.preview();
        }
        match resolved {
            Ok(resolved) => {
                self.resolved = Some(resolved);
                self.note = None;
            }
            Err(error) => self.note = Some(error),
        }
        Action::Continue
    }

    /// Creation failed. The draft is kept exactly as it was: the operator's edits
    /// are the expensive part, and a refused creation minted nothing to reconcile
    /// with (docs/control-api.md).
    pub(crate) fn creation_failed(&mut self, message: String) {
        self.creating = false;
        self.page = Page::Form;
        self.note = Some(message);
    }

    /// Recent models failed to load. The menu stays open on its typed-ID row:
    /// an empty answer and a failed one read the same, and closing the menu
    /// would punish the operator for a read that was only ever a shortcut.
    pub(crate) fn note_recents_failed(&mut self, message: String) {
        self.note = Some(message);
    }

    /// Ask for a fresh preview of the current draft, recording what was sent so
    /// a reply that arrives after the next edit can be recognized as stale.
    fn preview(&mut self) -> Action {
        let options = self.draft.to_options();
        self.pending = Some(options.clone());
        Action::Preview(options)
    }

    fn paste(&mut self, text: &str) {
        if let Page::Text { value, .. } | Page::Temperature { value } = &mut self.page {
            value.paste(text);
        }
    }

    /// The connections a role may choose from. Every configured connection is
    /// listed, including an unavailable one: omitting it would present a
    /// credential problem as a connection that was never configured, and the
    /// host says which it is (docs/control-api.md).
    fn connections(&self) -> &[ConnectionInventory] {
        &self.context.connections
    }

    /// The connection index the open Models menu was opened on. Zero when no
    /// menu is open — the value is only read beside a Models page.
    fn model_connection(&self) -> usize {
        match &self.page {
            Page::Models { connection, .. } => *connection,
            _ => 0,
        }
    }

    pub(super) fn handle(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::Continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Cancel;
        }
        if key.code == KeyCode::Esc {
            self.page = match &self.page {
                Page::Form => return Action::Cancel,
                Page::Models {
                    role, connection, ..
                } => Page::Connections {
                    role: *role,
                    selected: connection + self.leading_choices(*role),
                },
                Page::Text {
                    role, connection, ..
                } => Page::Models {
                    role: *role,
                    connection: *connection,
                    models: Vec::new(),
                    selected: 0,
                },
                _ => Page::Form,
            };
            self.note = None;
            return Action::Continue;
        }
        match self.page {
            Page::Form => self.handle_form_key(key),
            Page::Connections { role, .. } => {
                let len = self.connections().len() + self.leading_choices(role);
                if let Page::Connections { selected, .. } = &mut self.page {
                    move_selection(selected, len, key);
                }
                match key.code {
                    KeyCode::Enter => self.choose_connection(role),
                    _ => Action::Continue,
                }
            }
            Page::Models { role, .. } => {
                let len = match &self.page {
                    Page::Models { models, .. } => models.len() + 1,
                    _ => 1,
                };
                if let Page::Models { selected, .. } = &mut self.page {
                    move_selection(selected, len, key);
                }
                match key.code {
                    KeyCode::Enter => self.choose_model(role, self.model_connection()),
                    _ => Action::Continue,
                }
            }
            Page::Text { .. } => {
                if let Page::Text { value, .. } = &mut self.page {
                    value.handle_key(key);
                }
                match key.code {
                    KeyCode::Enter => self.accept_model_text(),
                    _ => Action::Continue,
                }
            }
            Page::Effort { .. } => {
                if let Page::Effort { selected } = &mut self.page {
                    move_selection(selected, EFFORTS.len(), key);
                }
                match (key.code, &self.page) {
                    (KeyCode::Enter, Page::Effort { selected }) => {
                        let selected = *selected;
                        self.draft.effort = (selected > 0).then(|| EFFORTS[selected].to_string());
                        self.page = Page::Form;
                        self.preview()
                    }
                    _ => Action::Continue,
                }
            }
            Page::Temperature { .. } => {
                if let Page::Temperature { value } = &mut self.page {
                    value.handle_key(key);
                }
                match key.code {
                    KeyCode::Enter => self.accept_temperature(),
                    _ => Action::Continue,
                }
            }
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> Action {
        match step(key) {
            Some(Step::Back) => self.selected = self.selected.saturating_sub(1),
            Some(Step::Forward) => self.selected = (self.selected + 1).min(5),
            None => {}
        }
        match key.code {
            KeyCode::Enter => {
                self.note = None;
                self.page = match self.selected {
                    0 => return self.start(),
                    1 => Page::Connections {
                        role: Role::Main,
                        selected: 0,
                    },
                    2 => Page::Effort {
                        selected: self
                            .draft
                            .effort
                            .as_ref()
                            .and_then(|s| EFFORTS.iter().position(|e| e == s))
                            .unwrap_or(0),
                    },
                    3 => Page::Temperature {
                        value: Line::new(
                            &self
                                .draft
                                .temperature
                                .map(|t| t.to_string())
                                .unwrap_or_default(),
                        ),
                    },
                    4 => Page::Connections {
                        role: Role::Spotter,
                        selected: 0,
                    },
                    _ => Page::Connections {
                        role: Role::Triage,
                        selected: 0,
                    },
                };
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                match self.selected {
                    1 => self.draft.main = None,
                    2 => self.draft.effort = None,
                    3 => self.draft.temperature = None,
                    4 => self.draft.spotter = None,
                    5 => self.draft.triage = TriageDraft::Inherit,
                    _ => return Action::Continue,
                }
                return self.preview();
            }
            _ => {}
        }
        Action::Continue
    }

    /// Enter on `Start session`. Refused while the host has not answered a
    /// preview, or has answered with something it would refuse anyway: posting a
    /// creation the host is known to reject spends a round trip to learn what is
    /// already on screen.
    fn start(&mut self) -> Action {
        if self.creating {
            return Action::Continue;
        }
        match &self.resolved {
            None => {
                self.note = Some("waiting for the host to resolve this draft…".into());
                Action::Continue
            }
            Some(resolved) if !resolved.issues.is_empty() => {
                self.note =
                    Some("this draft has unresolved fields — the marked ones need a value.".into());
                Action::Continue
            }
            Some(_) => {
                self.creating = true;
                self.note = Some("creating the session…".into());
                Action::Create(self.draft.to_options())
            }
        }
    }
}

/// A list move: the arrows and Tab, plus the emacs pair C-p / C-n.
enum Step {
    Back,
    Forward,
}

fn step(key: KeyEvent) -> Option<Step> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up | KeyCode::BackTab => Some(Step::Back),
        KeyCode::Down | KeyCode::Tab => Some(Step::Forward),
        KeyCode::Char('p') if ctrl => Some(Step::Back),
        KeyCode::Char('n') if ctrl => Some(Step::Forward),
        _ => None,
    }
}

fn move_selection(selected: &mut usize, len: usize, key: KeyEvent) {
    match step(key) {
        Some(Step::Back) => *selected = selected.saturating_sub(1),
        Some(Step::Forward) => *selected = (*selected + 1).min(len.saturating_sub(1)),
        None => {}
    }
}
