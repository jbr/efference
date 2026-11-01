//! Remembered launch locations — the directories a session has been started in,
//! keyed by working root plus the source configuration it was started under
//! (docs/control-api.md, docs/state-store.md).
//!
//! One row per identity, written by every successful creation
//! ([`crate::daemon::Host::create`]), so a launch is what puts a directory on the
//! list: there is no registration step and no nickname. A relaunch refreshes the
//! row's timestamp rather than adding one, which is what makes the index read as
//! most-recently-used and keeps one project's launches from crowding out another's.
//!
//! The identity is [`crate::config::history::Scope`], unchanged: a canonicalized working
//! root and the *template* configuration the boot consumed — never the per-session
//! snapshot, whose path is unique per session. `config` is stored beside it
//! because the key alone cannot say whether the configuration was named: an
//! explicit `efference.toml` and the probed one share a key, while
//! [`crate::config::load_in`] treats them differently — a named configuration that
//! is missing is an error, an absent default falls back to
//! [`crate::config::Config::default`]. That difference is what
//! [`Project::available`] checks and what the index hands a client back to create
//! with.
//!
//! The table is in the user state database ([`crate::platform::state`]) beside the model
//! picks and the catalog, every operation opening its own connection and every one
//! best-effort: a store that cannot be opened or written reads as an empty list,
//! and never fails the creation that was trying to remember itself.
//!
//! Availability is a read-time question, not a stored one. A directory on an
//! unmounted volume comes back when the volume does, so the index filters and
//! nothing else writes: only [`Projects::forget`] deletes, and it deletes the
//! remembered entry alone.

use std::path::{Path, PathBuf};

/// This table's half of the user state database ([`crate::platform::state`]).
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS remembered_projects (
        last_used_at INTEGER NOT NULL,
        cwd TEXT NOT NULL,
        config_key TEXT NOT NULL,
        config TEXT,
        PRIMARY KEY (cwd, config_key)
    );";

/// How many entries the index serves. A picker reads the top of the list, and an
/// unfiltered read of every directory ever launched in is not one anybody asked
/// for; [`Projects::forget`] takes the identity, so a capped index does not make
/// an entry unreachable.
pub const INDEX_LIMIT: usize = 50;

/// One remembered location: where a session was started, and under which
/// configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Unix seconds of the most recent creation here.
    pub last_used_at: i64,
    /// The working root, canonicalized — the `cwd` `POST /sessions` takes.
    pub cwd: PathBuf,
    /// The identity's configuration half ([`crate::config::history::Scope::config_key`]).
    pub config_key: String,
    /// The configuration the creation named, as it was named, or `None` when it
    /// named none and the probe in effect will run again. `POST /sessions` takes
    /// this value back unchanged.
    pub config: Option<PathBuf>,
}

impl Project {
    /// Whether a creation could still start here now — the question the index
    /// filters on, answered against the filesystem each time rather than stored,
    /// because the answer is a property of this moment.
    ///
    /// The rules are [`crate::config::load_in`]'s: a directory that is not there
    /// has nothing to run in, a *named* configuration that is not there is an
    /// error, and an unnamed one is not — creation falls back to its defaults, so
    /// a row recorded before the directory grew an `efference.toml` stays listed
    /// and stays launchable. This is the index's check, not creation's: a volume
    /// can unmount between the two, which is why creation validates again.
    ///
    /// Blocking by nature — a stat on an unresponsive network mount waits — so the
    /// index runs these off the executor.
    pub fn available(&self) -> bool {
        if !self.cwd.is_dir() {
            return false;
        }
        match &self.config {
            Some(config) => self.cwd.join(config).is_file(),
            None => true,
        }
    }

    /// What a picker shows without being told a name: the directory's own. Falls
    /// back to the full path where a basename says nothing — at a filesystem root
    /// there isn't one.
    pub fn label(&self) -> String {
        self.cwd.file_name().map_or_else(
            || self.cwd.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// The store: a path to `state.db`, opened per operation. A store with no path is
/// disabled — every read empty, every write dropped — which is what
/// [`crate::inference::InferenceRegistry`] hands a config that names no home
/// directory.
#[derive(Clone, Debug, Default)]
pub struct Projects {
    path: Option<PathBuf>,
}

impl Projects {
    /// Open the store at `path`, creating the database and this table on first
    /// use. `CREATE TABLE IF NOT EXISTS` is the migration story
    /// ([`crate::platform::state`]): a later table lands beside this one, and a later column
    /// in a follow-up change.
    pub fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// No store: every read is empty, every write is dropped.
    pub fn disabled() -> Self {
        Self::default()
    }

    fn connect(&self) -> Option<rusqlite::Connection> {
        crate::platform::state::open(self.path.as_ref()?, SCHEMA).ok()
    }

    /// Remember that a session started in `project`. An existing row for the same
    /// identity is refreshed, not duplicated. Best-effort: a store that cannot be
    /// written must never fail the creation remembering itself.
    pub fn record(&self, project: &Project) {
        let Some(connection) = self.connect() else {
            return;
        };
        connection
            .execute(
                "INSERT OR REPLACE INTO remembered_projects
                     (last_used_at, cwd, config_key, config)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    project.last_used_at,
                    project.cwd.to_string_lossy(),
                    project.config_key,
                    project
                        .config
                        .as_ref()
                        .map(|c| c.to_string_lossy().into_owned()),
                ],
            )
            .ok();
    }

    /// The stored entries, most recently used first, up to [`INDEX_LIMIT`].
    /// Availability is not applied here — that is the index's filter, and it is
    /// the reader's to make.
    pub fn recent(&self) -> Vec<Project> {
        let Some(connection) = self.connect() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT last_used_at, cwd, config_key, config FROM remembered_projects
             ORDER BY last_used_at DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        statement
            .query_map([INDEX_LIMIT as i64], |row| {
                Ok(Project {
                    last_used_at: row.get(0)?,
                    cwd: PathBuf::from(row.get::<_, String>(1)?),
                    config_key: row.get(2)?,
                    config: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
                })
            })
            .map_or_default(|rows| rows.filter_map(Result::ok).collect())
    }

    /// Every distinct remembered root, most recently used first, without the
    /// index's cap: the roots a session-id backfill scans (`crate::session_ids`).
    pub fn roots(&self) -> Vec<PathBuf> {
        let Some(connection) = self.connect() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT cwd FROM remembered_projects GROUP BY cwd ORDER BY MAX(last_used_at) DESC",
        ) else {
            return Vec::new();
        };
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_or_default(|rows| rows.filter_map(Result::ok).map(PathBuf::from).collect())
    }

    /// Whether `identity` — a `cwd` and a `config_key` as the index reported them
    /// — is remembered.
    pub fn contains(&self, cwd: &Path, config_key: &str) -> bool {
        self.connect().is_some_and(|connection| {
            connection
                .query_row(
                    "SELECT 1 FROM remembered_projects WHERE cwd = ?1 AND config_key = ?2",
                    rusqlite::params![cwd.to_string_lossy(), config_key],
                    |_| Ok(()),
                )
                .is_ok()
        })
    }

    /// Forget one entry, by the identity the index reported: whether it is gone.
    ///
    /// The identity arrives as stored rather than recomputed from a `cwd` and a
    /// configuration, because recomputing re-probes the filesystem — a directory
    /// that grew an `efference.toml` since the launch resolves to a different key,
    /// and the forget would report nothing forgotten while its row stayed on
    /// screen.
    ///
    /// Deletes the remembered entry and nothing else: no file, no directory, and no
    /// session that ran there.
    pub fn forget(&self, cwd: &Path, config_key: &str) -> bool {
        let Some(connection) = self.connect() else {
            return false;
        };
        connection
            .execute(
                "DELETE FROM remembered_projects WHERE cwd = ?1 AND config_key = ?2",
                rusqlite::params![cwd.to_string_lossy(), config_key],
            )
            .is_ok_and(|deleted| deleted > 0)
    }
}

#[cfg(test)]
mod test;
