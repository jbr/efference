//! Where each picker and editor sends the draft when it is committed
//! (docs/tui.md).
//!
//! Every method here ends one page: it folds the selection into the [`Draft`],
//! returns to the form, and asks for a fresh preview. That is the whole reason
//! the request rate follows selections rather than keystrokes — the typing that
//! happens *inside* a page never reaches these.

use super::*;

impl Form {
    /// How many entries precede the connection rows in a role's picker. Every
    /// role offers "use the default"; triage offers explicitly sharing main's
    /// client beside it, because that is a different answer from inheriting.
    pub(super) fn leading_choices(&self, role: Role) -> usize {
        match role {
            Role::Triage => 2,
            Role::Main | Role::Spotter => 1,
        }
    }

    pub(super) fn choose_connection(&mut self, role: Role) -> Action {
        let Page::Connections { selected, .. } = &self.page else {
            return Action::Continue;
        };
        let selected = *selected;
        let leading = self.leading_choices(role);
        if selected < leading {
            self.draft.set_role(role, None);
            if role == Role::Triage && selected == 1 {
                self.draft.triage = TriageDraft::Shared;
            }
            self.page = Page::Form;
            return self.preview();
        }
        let connection = selected - leading;
        let Some(row) = self.connections().get(connection) else {
            return Action::Continue;
        };
        // A provider this build has no compiled catalog for goes straight to the
        // native-id editor rather than presenting an empty menu as a failure
        // (backlog/session-model-discovery.md).
        self.page = if row.provider.suggested_models().is_empty() {
            Page::Text {
                role,
                connection,
                value: String::new(),
            }
        } else {
            Page::Models {
                role,
                connection,
                query: String::new(),
                selected: 0,
            }
        };
        Action::Continue
    }

    pub(super) fn choose_model(&mut self) -> Action {
        let Page::Models {
            role,
            connection,
            query,
            selected,
        } = &self.page
        else {
            return Action::Continue;
        };
        let (role, connection, selected) = (*role, *connection, *selected);
        let query = query.clone();
        match self.matching_models(connection).get(selected) {
            Some(model) => {
                let model = (*model).to_string();
                self.set_role_model(role, connection, model);
                self.page = Page::Form;
                self.preview()
            }
            // Past the last suggestion is the native-id escape hatch, seeded with
            // whatever was typed to filter.
            None => {
                self.page = Page::Text {
                    role,
                    connection,
                    value: query,
                };
                Action::Continue
            }
        }
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

    pub(super) fn matching_models(&self, connection: usize) -> Vec<&'static str> {
        let Some(row) = self.connections().get(connection) else {
            return Vec::new();
        };
        let query = match &self.page {
            Page::Models { query, .. } => query.to_lowercase(),
            _ => String::new(),
        };
        row.provider
            .suggested_models()
            .iter()
            .copied()
            .filter(|model| model.to_lowercase().contains(&query))
            .collect()
    }
}
