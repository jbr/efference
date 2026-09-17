//! Fixed-data session form experiment, shared by inline and embedded containers
//! (docs/tui.md). No host requests or provider validation occur here.

mod inline;
pub(super) mod render;
#[cfg(test)]
mod test;

pub use inline::run;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub(super) const HEIGHT: u16 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Main,
    Spotter,
    Triage,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Self::Main => "Main model",
            Self::Spotter => "Spotter model",
            Self::Triage => "Triage model",
        }
    }
}

/// Fixtures are suggestions, not a capability catalog or a validation policy.
const CONNECTIONS: &[(&str, &str, &[&str])] = &[
    (
        "work",
        "Anthropic",
        &["claude-sonnet-4-5", "claude-haiku-4-5", "claude-opus-4-1"],
    ),
    (
        "personal",
        "OpenAI",
        &["gpt-5.6", "gpt-5.6-luna", "gpt-6-astra"],
    ),
    ("router", "OpenRouter", &[]),
];
const EFFORTS: &[&str] = &["Use default", "low", "medium", "high"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Draft {
    main: Option<String>,
    spotter: Option<String>,
    triage: Option<String>,
    effort: Option<String>,
    temperature: Option<u8>,
}

impl Draft {
    fn selection(&self, role: Role) -> &str {
        match role {
            Role::Main => self.main.as_deref().unwrap_or("work/claude-sonnet-4-5"),
            Role::Spotter => self.spotter.as_deref().unwrap_or("work/claude-haiku-4-5"),
            Role::Triage => self.triage.as_deref().unwrap_or("Share main client"),
        }
    }

    fn set_model(&mut self, role: Role, selection: Option<String>) {
        *match role {
            Role::Main => &mut self.main,
            Role::Spotter => &mut self.spotter,
            Role::Triage => &mut self.triage,
        } = selection;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Form,
    Connections {
        role: Role,
        selected: usize,
    },
    Models {
        role: Role,
        connection: usize,
        query: String,
        selected: usize,
    },
    Text {
        role: Role,
        connection: usize,
        value: String,
    },
    Effort {
        selected: usize,
    },
    Temperature {
        value: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Form {
    draft: Draft,
    selected: usize,
    page: Page,
    note: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    Continue,
    Cancel,
    Start(Draft),
}

impl Default for Form {
    fn default() -> Self {
        Self {
            draft: Draft::default(),
            selected: 0,
            page: Page::Form,
            note: None,
        }
    }
}

impl Form {
    fn paste(&mut self, text: &str) {
        let target = match &mut self.page {
            Page::Models {
                query, selected, ..
            } => {
                *selected = 0;
                query
            }
            Page::Text { value, .. } | Page::Temperature { value } => value,
            _ => return,
        };
        target.extend(text.chars().filter(|c| !c.is_control()));
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
                }
                | Page::Text {
                    role, connection, ..
                } => Page::Connections {
                    role: *role,
                    selected: connection + 1,
                },
                _ => Page::Form,
            };
            self.note = None;
            return Action::Continue;
        }
        let typed = match key.code {
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(c)
            }
            _ => None,
        };
        match &mut self.page {
            Page::Form => match key.code {
                KeyCode::Up | KeyCode::BackTab => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1).min(5),
                KeyCode::Enter => {
                    self.note = None;
                    self.page = match self.selected {
                        0 => return Action::Start(self.draft.clone()),
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
                            value: self
                                .draft
                                .temperature
                                .map(|t| t.to_string())
                                .unwrap_or_default(),
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
                KeyCode::Char('r') => match self.selected {
                    1 => self.draft.main = None,
                    2 => self.draft.effort = None,
                    3 => self.draft.temperature = None,
                    4 => self.draft.spotter = None,
                    5 => self.draft.triage = None,
                    _ => {}
                },
                _ => {}
            },
            Page::Connections { role, selected } => {
                move_selection(selected, CONNECTIONS.len() + 1, key.code);
                if key.code == KeyCode::Enter {
                    if *selected == 0 {
                        self.draft.set_model(*role, None);
                        self.page = Page::Form;
                    } else {
                        let connection = *selected - 1;
                        self.page = if CONNECTIONS[connection].2.is_empty() {
                            Page::Text {
                                role: *role,
                                connection,
                                value: String::new(),
                            }
                        } else {
                            Page::Models {
                                role: *role,
                                connection,
                                query: String::new(),
                                selected: 0,
                            }
                        };
                    }
                }
            }
            Page::Models {
                role,
                connection,
                query,
                selected,
            } => {
                if let Some(c) = typed {
                    query.push(c);
                    *selected = 0;
                } else if key.code == KeyCode::Backspace {
                    query.pop();
                    *selected = 0;
                }
                let models = matching_models(*connection, query);
                move_selection(selected, models.len() + 1, key.code);
                if key.code == KeyCode::Enter {
                    if let Some(model) = models.get(*selected) {
                        self.draft.set_model(
                            *role,
                            Some(format!("{}/{model}", CONNECTIONS[*connection].0)),
                        );
                        self.page = Page::Form;
                    } else {
                        self.page = Page::Text {
                            role: *role,
                            connection: *connection,
                            value: query.clone(),
                        };
                    }
                }
            }
            Page::Text {
                role,
                connection,
                value,
            } => {
                edit_text(value, typed, key.code);
                if key.code == KeyCode::Enter {
                    if value.trim().is_empty() || value.chars().any(char::is_whitespace) {
                        self.note = Some("Enter a model ID without whitespace.".into());
                    } else {
                        self.draft.set_model(
                            *role,
                            Some(format!("{}/{}", CONNECTIONS[*connection].0, value)),
                        );
                        self.page = Page::Form;
                        self.note = None;
                    }
                }
            }
            Page::Effort { selected } => {
                move_selection(selected, EFFORTS.len(), key.code);
                if key.code == KeyCode::Enter {
                    self.draft.effort = (*selected > 0).then(|| EFFORTS[*selected].to_string());
                    self.page = Page::Form;
                }
            }
            Page::Temperature { value } => {
                edit_text(value, typed, key.code);
                if key.code == KeyCode::Enter {
                    match value.parse::<u8>() {
                        Ok(n) if n <= 100 => self.draft.temperature = Some(n),
                        _ if value.is_empty() => self.draft.temperature = None,
                        _ => {
                            self.note =
                                Some("Use 0–100, or leave empty for provider default.".into());
                            return Action::Continue;
                        }
                    }
                    self.page = Page::Form;
                    self.note = None;
                }
            }
        }
        Action::Continue
    }

    pub(super) fn creation_failed(&mut self, message: String) {
        self.page = Page::Form;
        self.note = Some(message);
    }
}

fn edit_text(value: &mut String, typed: Option<char>, key: KeyCode) {
    if let Some(c) = typed {
        value.push(c);
    } else if key == KeyCode::Backspace {
        value.pop();
    }
}

fn move_selection(selected: &mut usize, len: usize, key: KeyCode) {
    match key {
        KeyCode::Up | KeyCode::BackTab => *selected = selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Tab => *selected = (*selected + 1).min(len.saturating_sub(1)),
        _ => {}
    }
}

fn matching_models(connection: usize, query: &str) -> Vec<&'static str> {
    CONNECTIONS[connection]
        .2
        .iter()
        .copied()
        .filter(|model| model.to_lowercase().contains(&query.to_lowercase()))
        .collect()
}
