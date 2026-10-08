//! The model picker's keys and catalog updates (docs/tui.md).
//!
//! The input line is the path: chips for the connection (and, on OpenRouter, the
//! author) taken so far, then the query filtering what is under them. A query
//! that changes resets the selection to the first row. Rows and their order are
//! `picker_rows.rs`; the matching rule is `matcher.rs`.

use super::picker_rows::Target;
use super::*;
use crate::tui::inline_list::{ListKey, list_key};

impl Form {
    pub(super) fn handle_picker_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let rows = self.picker_rows();
        let Page::Picker {
            path,
            query,
            selected,
            ..
        } = &mut self.page
        else {
            return Action::Continue;
        };
        *selected = (*selected).min(rows.len().saturating_sub(1));
        let current = rows.get(*selected);
        match key.code {
            KeyCode::Enter => {
                let Some(target) = current.map(|row| row.target.clone()) else {
                    return Action::Continue;
                };
                return match target.is_interior() {
                    true => self.push_chip(target),
                    false => self.commit_pick(target),
                };
            }
            KeyCode::Tab => {
                let Some(row) = current else {
                    return Action::Continue;
                };
                if row.target.is_interior() {
                    let target = row.target.clone();
                    return self.push_chip(target);
                }
                if let Target::Model {
                    connection,
                    group,
                    model,
                } = &row.target
                {
                    // The leaf's ancestors become chips and its own id the query.
                    let own = match group {
                        Some(_) => matcher::strip_author(model),
                        None => model.clone(),
                    };
                    let mut chips = vec![Chip::Connection(connection.clone())];
                    chips.extend(group.clone().map(Chip::Group));
                    *path = chips;
                    *query = Line::new(&own);
                    *selected = 0;
                }
                return self.refresh_on_miss();
            }
            KeyCode::Char('/')
                if !ctrl && !alt && current.is_some_and(|r| r.target.is_interior()) =>
            {
                let target = current.map(|row| row.target.clone());
                return match target {
                    Some(target) => self.push_chip(target),
                    None => Action::Continue,
                };
            }
            KeyCode::Backspace if query.text().is_empty() => {
                path.pop();
                *selected = 0;
            }
            _ => {
                if list_key(query, selected, rows.len(), key) == ListKey::Edited {
                    return self.refresh_on_miss();
                }
            }
        }
        Action::Continue
    }

    /// Take an interior row into the path: its chip, the query cleared, the
    /// selection back at the first row. A group reached from the root brings its
    /// connection with it.
    fn push_chip(&mut self, target: Target) -> Action {
        let Page::Picker {
            path,
            query,
            selected,
            ..
        } = &mut self.page
        else {
            return Action::Continue;
        };
        match target {
            Target::Connection(connection) => *path = vec![Chip::Connection(connection)],
            Target::Group { connection, group } => {
                *path = vec![Chip::Connection(connection), Chip::Group(group)]
            }
            _ => return Action::Continue,
        }
        *query = Line::new("");
        *selected = 0;
        Action::Continue
    }

    /// Enter on a leaf: the role's draft takes the model, the form returns, and
    /// the host is asked to resolve the new draft.
    fn commit_pick(&mut self, target: Target) -> Action {
        let Page::Picker { role, .. } = &self.page else {
            return Action::Continue;
        };
        let role = *role;
        let (Target::Model {
            connection, model, ..
        }
        | Target::Typed { connection, model }) = target
        else {
            return Action::Continue;
        };
        self.set_role_model(role, &connection, model);
        self.page = Page::Form;
        self.note = None;
        self.preview()
    }

    /// A query under a connection chip that matches nothing asks the host for
    /// that connection's list once more, so a model released since the last read
    /// shows up by the time its id is typed. At most once per connection for one
    /// visit to the picker, and never for a connection with no credential.
    pub(super) fn refresh_on_miss(&mut self) -> Action {
        let Page::Picker {
            role,
            path,
            query,
            refreshed,
            ..
        } = &self.page
        else {
            return Action::Continue;
        };
        let Some(Chip::Connection(connection)) = path.first() else {
            return Action::Continue;
        };
        let text = query.text();
        if text.trim().is_empty()
            || refreshed.contains(connection)
            || self.connection_unavailable(connection)
        {
            return Action::Continue;
        }
        let (nodes, _) = self.tree(*role);
        if !matcher::rank(&nodes, path, &text, *role == Role::Main).is_empty() {
            return Action::Continue;
        }
        let connection = connection.clone();
        if let Page::Picker { refreshed, .. } = &mut self.page {
            refreshed.push(connection.clone());
        }
        Action::RefreshCatalog {
            connection,
            max_age: Some(CATALOG_MISS_MAX_AGE),
        }
    }

    /// One connection's catalog read landed: its entry replaces the held one, and
    /// the selection stays inside the rows that are there now. A failed read marks
    /// the entry failed and keeps whatever rows it had.
    pub(crate) fn catalog_returned(
        &mut self,
        connection: &str,
        result: Result<ConnectionCatalog, String>,
    ) {
        let position = self
            .context
            .catalog
            .iter()
            .position(|entry| entry.connection == connection);
        match (result, position) {
            (Ok(entry), Some(at)) => self.context.catalog[at] = entry,
            (Ok(entry), None) => self.context.catalog.push(entry),
            (Err(error), Some(at)) => {
                let entry = &mut self.context.catalog[at];
                entry.status = crate::client::CatalogStatus::Failed;
                entry.error = Some(error);
            }
            (Err(error), None) => {
                let Some(inventory) = self.connections().iter().find(|row| row.name == connection)
                else {
                    return;
                };
                self.context.catalog.push(ConnectionCatalog {
                    connection: connection.to_string(),
                    provider: inventory.provider,
                    status: crate::client::CatalogStatus::Failed,
                    age: None,
                    error: Some(error),
                    models: Vec::new(),
                });
            }
        }
        let len = self.picker_rows().len();
        if let Page::Picker { selected, .. } = &mut self.page {
            *selected = (*selected).min(len.saturating_sub(1));
        }
    }
}

#[cfg(test)]
mod test;
