//! The new-session form (docs/tui.md): the client half of the session-options
//! surface `QUERY /sessions` and `POST /sessions` serve (docs/control-api.md).
//!
//! Four host-owned inputs meet here and nowhere else — the connection inventory
//! from `GET /inference`, the model catalog from `GET /inference/catalog`
//! (docs/model-catalog.md; each connection's entry is replaced as its own read
//! lands), the effective draft `QUERY /sessions` recomputes after each edit, and
//! the creation the form finally posts. The form owns none of
//! them: it holds the operator's raw overrides ([`Draft`]) and the host's latest
//! answer about them ([`ResolvedOptions`]) side by side, and renders the
//! difference.
//!
//! Preview is recomputed on *committed* edits — leaving a picker or accepting an
//! editor — never per keystroke. Typing in the model picker's query and in the
//! text editors is local, so the request rate follows selections rather than
//! characters and no debounce timer is needed.

mod draft;
mod inline;
mod matcher;
mod pages;
mod picker;
mod picker_render;
mod picker_rows;
pub(super) mod render;
#[cfg(test)]
mod test;

pub use inline::{run, run_hosted};

pub(crate) use draft::Role;
pub(in crate::tui) use draft::{Draft, RoleDraft, TriageDraft};

use crate::client::ConnectionCatalog;
use crate::inference::{ConnectionInventory, ResolvedOptions, SessionOptions};
use crate::tui::inline_list::Line;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use matcher::Chip;
use std::path::PathBuf;

/// The form page's fixed rows: start, main, effort, temperature, spotter,
/// triage, worktree. Named because the height calculation and the cursor bound
/// are the same seven (`render.rs`).
const FORM_ROWS: usize = 7;

/// Rows the model picker gets before it scrolls. Sized so the root view — the
/// top recents and every connection of a typical host — fits without scrolling
/// (`picker.rs`).
const MENU_ROWS: usize = 12;

/// The most rows the picker's viewport reserves, however many connections a
/// host lists.
const MAX_PICKER_ROWS: usize = 20;

/// How stale a copy may be before a no-match query reads the connection again
/// (`picker.rs`). The host raises anything under a minute to a minute.
const CATALOG_MISS_MAX_AGE: u64 = 60;

const EFFORTS: &[&str] = &["Use default", "low", "medium", "high", "xhigh", "max"];

/// Where a created session would run, and what the host has to offer for it.
/// The client that opened the form supplies both: the working root is that
/// client's own, and so is the environment creation will carry — which is why
/// the form displays this rather than asking, and why it cannot be edited until
/// there is a way to capture an environment at an arbitrary directory
/// (backlog/new-session-form.md).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NewSessionContext {
    pub(crate) cwd: PathBuf,
    /// `GET /inference`, sorted by name. Empty until the first read lands, which
    /// the form states rather than rendering as an empty menu.
    pub(crate) connections: Vec<ConnectionInventory>,
    /// `GET /inference/catalog` for the same root: each connection's listing
    /// with its status and the recent ranks. Empty when the read failed or no
    /// host is behind the form — the picker then offers the connections and
    /// the typed-id path only.
    pub(crate) catalog: Vec<ConnectionCatalog>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Form,
    /// The one model picker (`picker.rs`): a typeahead over the catalog tree. The
    /// chips are the path taken so far; `query` filters what is under them.
    Picker {
        role: Role,
        path: Vec<Chip>,
        query: Line,
        selected: usize,
        /// Connections whose no-match refresh was already requested during this
        /// visit, so a miss asks the host at most once per connection.
        refreshed: Vec<String>,
    },
    Effort {
        selected: usize,
    },
    Temperature {
        value: Line,
    },
}

#[derive(Clone, Debug, PartialEq)]
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
    /// environment, which never enter the UI thread (docs/env.md). `worktree` is
    /// creation-only — the options in `options` are exactly what a preview
    /// resolves, and the host resolves no worktree value (docs/worktrees.md).
    Create {
        options: SessionOptions,
        worktree: bool,
    },
    /// Read one connection's catalog entry again, awaiting the provider when
    /// the stored copy is older than `max_age` seconds. Answered with
    /// [`Form::catalog_returned`].
    RefreshCatalog {
        connection: String,
        max_age: Option<u64>,
    },
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

    /// Ask for a fresh preview of the current draft, recording what was sent so
    /// a reply that arrives after the next edit can be recognized as stale.
    fn preview(&mut self) -> Action {
        let options = self.draft.to_options();
        self.pending = Some(options.clone());
        Action::Preview(options)
    }

    pub(super) fn paste(&mut self, text: &str) -> Action {
        match &mut self.page {
            Page::Temperature { value } => value.paste(text),
            Page::Picker {
                query, selected, ..
            } => {
                query.paste(text);
                *selected = 0;
                return self.refresh_on_miss();
            }
            _ => {}
        }
        Action::Continue
    }

    /// The connections a role may choose from. Every configured connection is
    /// listed, including an unavailable one: omitting it would present a
    /// credential problem as a connection that was never configured, and the
    /// host says which it is (docs/control-api.md).
    fn connections(&self) -> &[ConnectionInventory] {
        &self.context.connections
    }

    pub(super) fn handle(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::Continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Cancel;
        }
        if key.code == KeyCode::Esc {
            self.note = None;
            match &mut self.page {
                Page::Form => return Action::Cancel,
                Page::Picker {
                    query, selected, ..
                } if !query.text().is_empty() => {
                    *query = Line::new("");
                    *selected = 0;
                }
                _ => self.page = Page::Form,
            }
            return Action::Continue;
        }
        match self.page {
            Page::Form => self.handle_form_key(key),
            Page::Picker { .. } => self.handle_picker_key(key),
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
            Some(Step::Forward) => self.selected = (self.selected + 1).min(FORM_ROWS - 1),
            None => {}
        }
        match key.code {
            KeyCode::Enter => {
                self.note = None;
                self.page = match self.selected {
                    0 => return self.start(),
                    1 => Self::picker(Role::Main),
                    2 => Page::Effort {
                        selected: self
                            .draft
                            .effort
                            .as_ref()
                            .and_then(|s| EFFORTS.iter().position(|e| e == s))
                            .unwrap_or(0),
                    },
                    3 => Page::Temperature {
                        value: Line::new(&self.draft.temperature.map_or_default(|t| t.to_string())),
                    },
                    4 => Self::picker(Role::Spotter),
                    // A boolean: no page, and no preview — the host resolves no
                    // worktree value to re-answer (docs/worktrees.md).
                    6 => {
                        self.draft.worktree = !self.draft.worktree;
                        return Action::Continue;
                    }
                    // The triage row, and the fallback for an index past the list.
                    _ => Self::picker(Role::Triage),
                };
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                match self.selected {
                    1 => self.draft.main = None,
                    2 => self.draft.effort = None,
                    3 => self.draft.temperature = None,
                    4 => self.draft.spotter = None,
                    5 => self.draft.triage = TriageDraft::Inherit,
                    // The worktree knob is client-only, so its reset needs no
                    // preview to reconcile.
                    6 => {
                        self.draft.worktree = false;
                        return Action::Continue;
                    }
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
                Action::Create {
                    options: self.draft.to_options(),
                    worktree: self.draft.worktree,
                }
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
