//! What the session-control verbs read off the host (docs/tools.md): a session's
//! status whether it is starting or hosted, the sessions an archive ends, each
//! reader's transcript cursor, and what the host offers when a creation names a model
//! it cannot run.

use super::*;
use crate::protocol::status::StatusView;

/// The most model ids an offer names per connection.
const OFFERED_MODELS: usize = 8;

impl Host {
    /// `id`'s status view, starting or hosted; `None` when the host holds neither.
    pub(crate) fn status_view_of(&self, id: &str) -> Option<StatusView> {
        match self.entry(id)? {
            Entry::Hosted(hosted) => Some(hosted.session.status_view(id, &hosted.session.status())),
            Entry::Starting(starting) => starting.status_view(),
        }
    }

    /// The sessions that end when `id` does (docs/tools.md): the children it created
    /// that are not detached, theirs in turn, and so on, in the order the cascade
    /// reaches them.
    pub(crate) fn ending_with(&self, id: &str) -> Vec<String> {
        let mut ending = Vec::new();
        let mut next = vec![id.to_string()];
        while let Some(parent) = next.pop() {
            for child in self.cascade_of(&parent) {
                if !ending.contains(&child) && child != id {
                    next.push(child.clone());
                    ending.push(child);
                }
            }
        }
        ending
    }

    /// The last seq `reader` was shown of `target`'s transcript.
    pub(crate) fn read_cursor(&self, target: &str, reader: &str) -> Option<Seq> {
        self.sessions_mut()
            .hosted(target)
            .and_then(|hosted| hosted.read_cursors.get(reader).copied())
    }

    /// Record the last seq `reader` was shown of `target`'s transcript.
    pub(crate) fn set_read_cursor(&self, target: &str, reader: &str, seq: Seq) {
        if let Some(hosted) = self.sessions_mut().hosted_mut(target) {
            hosted.read_cursors.insert(reader.to_string(), seq);
        }
    }

    /// What this host can run, for a creation whose model it cannot (docs/model-catalog.md):
    /// each connection, whether it is usable, and the first ids of its held listing. The
    /// listing is suggestions and is never fetched here; a connection with none named
    /// still takes any id its provider serves.
    pub(crate) fn offered_models(&self) -> String {
        let connections = self.inference.inventory();
        if connections.is_empty() {
            return "this host has no connections configured".to_string();
        }
        let now = crate::config::history::now();
        let offered: Vec<String> = connections
            .iter()
            .map(|connection| {
                if !connection.available {
                    return format!("{} (unavailable)", connection.name);
                }
                let rows = self
                    .catalog
                    .snapshot(&connection.name, connection.provider, now)
                    .rows
                    .unwrap_or_default();
                let mut models: Vec<String> = rows
                    .iter()
                    .take(OFFERED_MODELS)
                    .map(|row| format!("{}/{}", connection.name, row.model))
                    .collect();
                if rows.len() > OFFERED_MODELS {
                    models.push(format!("{} more", rows.len() - OFFERED_MODELS));
                }
                match models.is_empty() {
                    true => format!("{}/<model>", connection.name),
                    false => models.join(", "),
                }
            })
            .collect();
        format!("this host offers {}", offered.join("; "))
    }
}
