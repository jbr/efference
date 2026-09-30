//! Where each picker and editor sends the draft when it is committed
//! (docs/tui.md).
//!
//! Every method here ends one page: it folds the selection into the [`Draft`],
//! returns to the form, and asks for a fresh preview. That is the whole reason
//! the request rate follows selections rather than keystrokes — the typing that
//! happens *inside* a page never reaches these.

use super::*;

impl Form {
    /// The recent picks shown in a role's connection picker: past picks across
    /// all connections, most decayed weight first, naming only connections the
    /// host still lists. A pick on a retired connection stays out of the menu —
    /// preview would refuse it, and the per-connection recent menu already
    /// covers the typed-ID path for anything unlisted.
    pub(super) fn picker_recents(&self) -> Vec<&crate::client::RecentPick> {
        self.context
            .recent_picks
            .iter()
            .filter(|pick| {
                self.connections()
                    .iter()
                    .any(|row| row.name == pick.connection)
            })
            .collect()
    }

    /// How many entries precede the connection rows in a role's picker. Every
    /// role offers "use the default" (and triage offers explicitly sharing
    /// main's client beside it), followed by the recent picks across all
    /// connections — so one menu covers every provider without opening each
    /// connection's own recent menu first.
    pub(super) fn leading_choices(&self, role: Role) -> usize {
        let base = match role {
            Role::Triage => 2,
            Role::Main | Role::Spotter => 1,
        };
        base + self.picker_recents().len()
    }

    pub(crate) fn choose_connection(&mut self, role: Role) -> Action {
        let Page::Connections { selected, .. } = &self.page else {
            return Action::Continue;
        };
        let selected = *selected;
        let base = match role {
            Role::Triage => 2,
            Role::Main | Role::Spotter => 1,
        };
        if selected < base {
            self.draft.set_role(role, None);
            if role == Role::Triage && selected == 1 {
                self.draft.triage = TriageDraft::Shared;
            }
            self.page = Page::Form;
            return self.preview();
        }
        let recents = self.picker_recents();
        if selected < base + recents.len() {
            let pick = recents[selected - base];
            self.draft.set_role(
                role,
                Some(RoleDraft {
                    connection: pick.connection.clone(),
                    model: pick.model.clone(),
                }),
            );
            self.page = Page::Form;
            return self.preview();
        }
        let connection = selected - base - recents.len();
        // A connection opens its recent models first: past picks on it, most
        // decayed weight first, with the typed-ID editor as the last row. The
        // host's discovery route merges its catalog into this same menu when it
        // lands (backlog/session-model-discovery.md); the typed path stays in
        // every case.
        self.page = Page::Models {
            role,
            connection,
            models: Vec::new(),
            selected: 0,
        };
        Action::Recents(RecentsRequest { role, connection })
    }

    /// Fold the host's recent-models answer into the open menu. Dropped when
    /// the operator has moved on — the answer describes a menu nobody is
    /// looking at, the same staleness rule preview follows.
    pub(crate) fn recents_returned(
        &mut self,
        role: Role,
        connection: usize,
        models: Vec<crate::client::ModelSuggestion>,
    ) -> Action {
        let Page::Models {
            role: page_role,
            connection: page_connection,
            ..
        } = &self.page
        else {
            return Action::Continue;
        };
        if *page_role != role || *page_connection != connection {
            return Action::Continue;
        }
        if let Page::Models {
            models: current,
            selected,
            ..
        } = &mut self.page
        {
            *current = models;
            *selected = (*selected).min(current.len());
        }
        Action::Continue
    }

    pub(super) fn choose_model(&mut self, role: Role, connection: usize) -> Action {
        let Page::Models {
            models, selected, ..
        } = &self.page
        else {
            return Action::Continue;
        };
        let (role, connection) = (role, connection);
        // The last row is the typed-ID escape hatch; any other row is a
        // remembered pick, folded straight into the draft.
        if *selected >= models.len() {
            self.page = Page::Text {
                role,
                connection,
                value: String::new(),
            };
            return Action::Continue;
        }
        let Some(suggestion) = models.get(*selected) else {
            return Action::Continue;
        };
        let model = suggestion.model.clone();
        self.set_role_model(role, connection, model);
        self.page = Page::Form;
        self.note = None;
        self.preview()
    }

    pub(super) fn accept_model_text(&mut self) -> Action {
        let Page::Text {
            role,
            connection,
            value,
        } = &self.page
        else {
            return Action::Continue;
        };
        let (role, connection) = (*role, *connection);
        let value = value.trim().to_string();
        // The only check the client makes: a model id is one token, and the
        // whitespace case is a paste accident rather than something worth a round
        // trip. Whether the id *exists* is the host's answer, not this one.
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            self.note = Some("Enter a model ID without whitespace.".into());
            return Action::Continue;
        }
        self.set_role_model(role, connection, value);
        self.page = Page::Form;
        self.note = None;
        self.preview()
    }

    pub(super) fn accept_temperature(&mut self) -> Action {
        let Page::Temperature { value } = &self.page else {
            return Action::Continue;
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            self.draft.temperature = None;
        } else {
            match value.parse::<u8>() {
                Ok(n) if crate::inference::TEMPERATURE.contains(&n) => {
                    self.draft.temperature = Some(n)
                }
                _ => {
                    self.note = Some(format!(
                        "Use {}–{}, or leave empty for the provider default.",
                        crate::inference::TEMPERATURE.start(),
                        crate::inference::TEMPERATURE.end()
                    ));
                    return Action::Continue;
                }
            }
        }
        self.page = Page::Form;
        self.note = None;
        self.preview()
    }

    pub(super) fn set_role_model(&mut self, role: Role, connection: usize, model: String) {
        let Some(row) = self.connections().get(connection) else {
            return;
        };
        let selection = RoleDraft {
            connection: row.name.clone(),
            model,
        };
        self.draft.set_role(role, Some(selection));
    }
}
