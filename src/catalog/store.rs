//! The `model_catalog` table of the user state database (docs/model-catalog.md).
//!
//! One row per connection. Like the history store beside it, every operation
//! opens its own connection and is best-effort: a database that cannot be opened
//! or written reads as no stored copy, and the in-memory copy still serves.

use super::Stored;
use std::path::Path;

fn connect(path: &Path) -> Option<rusqlite::Connection> {
    crate::state::open(
        path,
        "CREATE TABLE IF NOT EXISTS model_catalog (
            connection TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            fetched_at INTEGER NOT NULL,
            rows TEXT NOT NULL
        );",
    )
    .ok()
}

/// The stored copy for `connection`. A row that does not parse is absent.
pub(super) fn load(path: &Path, connection: &str) -> Option<Stored> {
    let db = connect(path)?;
    db.query_row(
        "SELECT provider, fetched_at, rows FROM model_catalog WHERE connection = ?1",
        [connection],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )
    .ok()
    .and_then(|(provider, fetched_at, rows)| {
        Some(Stored {
            provider,
            fetched_at,
            rows: serde_json::from_str(&rows).ok()?,
        })
    })
}

/// Replace the stored copy for `connection`.
pub(super) fn save(path: &Path, connection: &str, stored: &Stored) {
    let Some(db) = connect(path) else {
        return;
    };
    let Ok(rows) = serde_json::to_string(&stored.rows) else {
        return;
    };
    db.execute(
        "INSERT OR REPLACE INTO model_catalog (connection, provider, fetched_at, rows)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![connection, stored.provider, stored.fetched_at, rows],
    )
    .ok();
}

/// Drop the stored copy for `connection`.
pub(super) fn delete(path: &Path, connection: &str) {
    if let Some(db) = connect(path) {
        db.execute(
            "DELETE FROM model_catalog WHERE connection = ?1",
            [connection],
        )
        .ok();
    }
}
