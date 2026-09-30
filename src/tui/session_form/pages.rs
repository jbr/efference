//! Where each picker and editor sends the draft when it is committed
//! (docs/tui.md).
//!
//! Every method here ends one page: it folds the selection into the [`Draft`],
//! returns to the form, and asks for a fresh preview. That is the whole reason
//! the request rate follows selections rather than keystrokes — the typing that
//! happens *inside* a page never reaches these.

use super::*;

impl Form {
    /// The recent picks shown in the main role's connection picker: past main
    /// picks across all connections, most decayed weight first, naming only
    /// connections the host still lists. Main-only because the roles want
    /// different qualities — spotter is the smallest trustworthy model, not the
    /// agent the operator kept picking — and should grow different histories
    /// rather than share main's. A pick on a retired connection stays out of
    /// the menu: preview would refuse it, and the per-connection recent menu
    /// already covers the typed-ID path for anything unlisted.
    pub(super) fn picker_recents(&self, role: Role) -> Vec<&crate::client::RecentPick> {
        if role != Role::Main {
            return Vec::new();
        }
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
    /// role leads with exactly one inherit row — for triage that row shares
    /// the main client — and the main role's menu then lists the recent picks
    /// across all connections, so one menu covers every provider without
    /// opening each connection's own recent menu first.
    pub(super) fn leading_choices(&self, role: Role) -> usize {
        1 + self.picker_recents(role).len()
    }

    pub(crate) fn choose_connection(&mut self, role: Role) -> Action {
        let Page::Connections { selected, .. } = &self.page else {
            return Action::Continue;
        };
        let selected = *selected;
        // The one inherit row clears the role back to its default — for triage
        // that is sharing the main client.
        const INHERIT_ROWS: usize = 1;
        if selected < INHERIT_ROWS {
            self.draft.set_role(role, None);
            self.page = Page::Form;
            return self.preview();
        }
        let recents = self.picker_recents(role);
        if selected < INHERIT_ROWS + recents.len() {
            let pick = recents[selected - INHERIT_ROWS];
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
        let connection = selected - INHERIT_ROWS - recents.len();
        // A connection opens its recent models first: past picks on it, most
        // decayed weight first, then the provider's listing (merged by the host
        // dispatcher), with the typed-ID editor as the last row in every case.
        self.page = Page::Models {
            role,
            connection,
            models: Vec::new(),
            authors: Vec::new(),
            author: None,
            selected: 0,
            loading: true,
        };
        Action::Recents(RecentsRequest {
            role,
            connection,
            author: None,
        })
    }

    /// Fold the host's answer into the open menu. Dropped when the operator has
    /// moved on — the answer describes a menu nobody is looking at, the same
    /// staleness rule preview follows.
    pub(crate) fn recents_returned(&mut self, answer: MenuAnswer) -> Action {
        let Page::Models {
            role,
            connection,
            author,
            models,
            authors,
            selected,
            loading,
        } = &mut self.page
        else {
            return Action::Continue;
        };
        if *role != answer.role || *connection != answer.connection || *author != answer.author {
            return Action::Continue;
        }
        *loading = false;
        *models = answer.models;
        *authors = answer.authors;
        *selected = (*selected).min(models.len() + authors.len());
        Action::Continue
    }

    pub(super) fn choose_model(&mut self, role: Role, connection: usize) -> Action {
        let Page::Models {
            models,
            authors,
            selected,
            ..
        } = &self.page
        else {
            return Action::Continue;
        };
        let (role, connection) = (role, connection);
        // Rows run recents and models, then authors, then the typed-ID escape
        // hatch. A model row folds straight into the draft; an author row opens
        // that author's models.
        if *selected >= models.len() + authors.len() {
            self.page = Page::Text {
                role,
                connection,
                value: Line::new(""),
            };
            return Action::Continue;
        }
        if let Some(author) = selected
            .checked_sub(models.len())
            .and_then(|index| authors.get(index))
        {
            let author = author.model.clone();
            self.page = Page::Models {
                role,
                connection,
                models: Vec::new(),
                authors: Vec::new(),
                author: Some(author.clone()),
                selected: 0,
                loading: true,
            };
            return Action::Recents(RecentsRequest {
                role,
                connection,
                author: Some(author),
            });
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
        let value = value.text().trim().to_string();
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
        let value = value.text().trim().to_string();
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
