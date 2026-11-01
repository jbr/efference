//! The host-wide register of session ids, the `session_ids` table of the user
//! state database (docs/session-title.md, docs/state-store.md).
//!
//! A session's id is its directory name under one project's `.efference`, so two
//! projects can each hold a directory of the same name while the host keys its
//! sessions by id alone. The table makes the id one host-wide name: a row per id
//! and the root that holds it. Minting inserts the row, which is what makes a
//! reservation atomic — the primary key refuses the second of two concurrent
//! creates — and an id stays reserved while its session is hosted, dormant, or
//! archived. Only a discard of an empty session releases a row
//! (docs/resumption.md).
//!
//! [`SessionIds::backfill`] fills the table from the session directories already
//! on disk under the remembered roots. An id found under two roots keeps its first
//! row and is reported as a [`Collision`] rather than failing anything.
//!
//! Every operation opens its own connection like the other tables
//! ([`crate::platform::state`]) and is best-effort: a store that cannot be opened reserves
//! nothing and refuses nothing, leaving the live-map and on-disk checks to stand
//! alone, as they did before the table existed.

use std::path::{Path, PathBuf};

/// This table's half of the user state database ([`crate::platform::state`]).
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS session_ids (
        id TEXT PRIMARY KEY,
        root TEXT NOT NULL
    );";

/// A reservation that was refused: the root already holding the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub root: PathBuf,
}

/// One id found under a second root during a backfill. The first row stands;
/// `other` is the root that lost it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub id: String,
    pub holder: PathBuf,
    pub other: PathBuf,
}

/// The register: a path to `state.db`, opened per operation. A register with no
/// path is disabled and reserves nothing.
#[derive(Clone, Debug, Default)]
pub struct SessionIds {
    path: Option<PathBuf>,
}

impl SessionIds {
    pub fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub fn disabled() -> Self {
        Self::default()
    }

    fn connect(&self) -> Option<rusqlite::Connection> {
        crate::platform::state::open(self.path.as_ref()?, SCHEMA).ok()
    }

    /// Reserve `id` for `root`: inserted when free, refused with the holder
    /// otherwise — including when the holder is `root` itself, since a creation
    /// names a session that does not exist yet. A register that cannot be opened
    /// reserves nothing and refuses nothing.
    pub fn reserve(&self, id: &str, root: &Path) -> Result<(), Held> {
        let Some(connection) = self.connect() else {
            return Ok(());
        };
        match insert(&connection, id, root) {
            Ok(true) | Err(_) => Ok(()),
            Ok(false) => Err(Held {
                root: holder(&connection, id).unwrap_or_default(),
            }),
        }
    }

    /// Reserve `id` for `root` if free, and accept it when `root` already holds
    /// it: the check a resume makes, where the session exists and the question is
    /// only whether another root owns the name.
    pub fn claim(&self, id: &str, root: &Path) -> Result<(), Held> {
        match self.reserve(id, root) {
            Err(held) if held.root != root => Err(held),
            _ => Ok(()),
        }
    }

    /// The root holding `id`, if any.
    pub fn holder(&self, id: &str) -> Option<PathBuf> {
        holder(&self.connect()?, id)
    }

    /// Drop `id`'s row, but only while `root` holds it. Used for a creation that
    /// reserved an id and then did not start a session.
    pub fn release(&self, id: &str, root: &Path) {
        if let Some(connection) = self.connect() {
            connection
                .execute(
                    "DELETE FROM session_ids WHERE id = ?1 AND root = ?2",
                    rusqlite::params![id, root.to_string_lossy()],
                )
                .ok();
        }
    }

    /// Register every session directory under each root's `.efference` that has
    /// not been registered. Returns the ids that turned up under a root other than
    /// the one holding them; nothing here fails.
    pub fn backfill(&self, roots: &[PathBuf]) -> Vec<Collision> {
        let Some(connection) = self.connect() else {
            return Vec::new();
        };
        let mut collisions = Vec::new();
        for root in roots {
            let base = root.join(crate::record::session_dir::SESSION_BASE);
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            let mut ids: Vec<String> = entries
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .join(crate::record::session_dir::LOG_NAME)
                        .is_file()
                })
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|id| crate::record::session_dir::plain_id(id))
                .collect();
            ids.sort();
            for id in ids {
                if let Ok(false) = insert(&connection, &id, root)
                    && let Some(holder) = holder(&connection, &id)
                    && holder != *root
                {
                    collisions.push(Collision {
                        id,
                        holder,
                        other: root.clone(),
                    });
                }
            }
        }
        collisions
    }
}

/// Whether the row was inserted.
fn insert(connection: &rusqlite::Connection, id: &str, root: &Path) -> rusqlite::Result<bool> {
    connection
        .execute(
            "INSERT OR IGNORE INTO session_ids (id, root) VALUES (?1, ?2)",
            rusqlite::params![id, root.to_string_lossy()],
        )
        .map(|inserted| inserted > 0)
}

fn holder(connection: &rusqlite::Connection, id: &str) -> Option<PathBuf> {
    connection
        .query_row("SELECT root FROM session_ids WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .map(PathBuf::from)
}

#[cfg(test)]
mod test;
