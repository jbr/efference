//! The user state database, `~/.efference/state.db` (docs/state-store.md).
//!
//! One file under the home directory holding the host's persistent state that is
//! not a secret: model picks, each connection's model listing, push
//! subscriptions, and the TOTP burn ledger. The values that *are* secrets — the
//! machine root, API keys, OAuth grants — live in the OS credential store
//! instead (docs/secrets.md).
//!
//! Each table's own module owns its schema and opens its own connection per
//! operation, passing its `CREATE TABLE IF NOT EXISTS` statements to [`open`].
//! That way a table that cannot be opened is that table's problem alone: the
//! best-effort callers fold the error away and read as empty, while the one
//! caller that cannot — the burn ledger, where a code it cannot record is a code
//! it cannot prove single-use — reports it and fails the code.
//!
//! Applying the schema on every open is the migration story. A later table lands
//! beside the existing ones in its own module; a later column lands as an
//! `ALTER TABLE` in a follow-up change. Nothing is read back at startup, so
//! there is no version to number.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the database lives relative to a home or project directory.
pub const RELATIVE_PATH: &str = ".efference/state.db";

/// How long a writer waits on another writer's lock before reporting the
/// database locked. Every daemon under the user shares this file, and one of the
/// tables fails closed on a write it cannot make — so a busy database has to cost
/// a wait rather than a rejected request.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The database beside `dir` — where a config that named its own directory keeps
/// its state, so a relocated config carries the state along with it.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(RELATIVE_PATH)
}

/// The user-scoped database. Falls back to a path relative to the current
/// directory when the platform reports no home directory: weaker (per-repo
/// state, and two daemons in different repos would not share it) but never
/// silently absent.
pub fn user_scoped() -> PathBuf {
    match std::env::home_dir() {
        Some(home) => path_in(&home),
        None => PathBuf::from(RELATIVE_PATH),
    }
}

/// Open the database at `path`, creating its parent directory and applying
/// `schema` — the caller's own `CREATE TABLE IF NOT EXISTS` statements, which
/// are also how its table arrives in a database the other tables already made.
pub fn open(path: &Path, schema: &str) -> Result<Connection, Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch(schema)?;
    Ok(connection)
}

/// Why the state database could not be opened, created, or written.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The directory could not be created.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// sqlite refused: a path that is not a database, a schema statement that
    /// does not parse, a lock held past [`BUSY_TIMEOUT`], a full disk.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[cfg(test)]
mod test;
