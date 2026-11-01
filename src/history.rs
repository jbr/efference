//! Explicit model picks, remembered per project (docs/transport.md).
//!
//! The host writes one row per session creation — only for an explicit human
//! pick (`SelectionSource::Request`), never an inherited default, so the
//! default cannot reinforce itself. Two reads consume it: the hierarchical
//! model default in `resolve_main`'s omitted branch (project mode past a mass
//! threshold, else the system-wide aggregate), and the usage-ordered recent
//! menu in every model picker, scoped to the selected connection.
//!
//! One exponential decay (half-life 30 days) serves both reads: lifetime counts
//! would pin a stale default on a churny model distribution, and the menu
//! absorbs ranking error (a slightly-wrong order costs a keypress, not a wrong
//! session), so a single parameter is deliberately all there is.
//!
//! Storage is the `model_picks` table of the user state database
//! ([`crate::platform::state`], docs/state-store.md) — no jsonl interim. Every operation
//! opens its own connection: writes happen once per session creation and reads
//! once per preview, so a pooled or shared connection would buy nothing and
//! `Connection` is `Send` but not `Sync`. Every operation is best-effort: a
//! store that cannot be opened or written reads as empty history, never a failed
//! creation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// This table's half of the user state database
/// ([`crate::platform::state`], docs/state-store.md).
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS model_picks (
        at INTEGER NOT NULL,
        cwd TEXT NOT NULL,
        config_key TEXT NOT NULL,
        connection TEXT NOT NULL,
        provider TEXT NOT NULL,
        model TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS model_picks_lookup
        ON model_picks (connection, cwd, config_key, at);
    CREATE TABLE IF NOT EXISTS triage_picks (
        at INTEGER NOT NULL,
        cwd TEXT NOT NULL,
        config_key TEXT NOT NULL,
        main_connection TEXT NOT NULL,
        main_model TEXT NOT NULL,
        triage_connection TEXT NOT NULL,
        triage_model TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS triage_picks_lookup
        ON triage_picks (main_connection, main_model, at);";

/// Single exponential decay, half-life 30 days. Eyeballed, not tuned: ~200
/// sessions is too little to fit per-person coefficients without overfitting a
/// drifting answer (backlog/model-selection-history.md).
pub const HALF_LIFE_DAYS: f64 = 30.0;

/// Decayed project mass that elects the project mode as the model default.
/// Eyeballed: about five effective explicit selections after decay.
pub const PROJECT_MASS_THRESHOLD: f64 = 5.0;

/// `config_key` when no origin config file was found: the built-in
/// [`Config::default`](crate::config::Config) is in effect, so there is no
/// path to key the picks on.
pub const BUILTIN_CONFIG_KEY: &str = "builtin-default";

/// One remembered pick: what was chosen, where, and in which config's name.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// Unix seconds when the session was created.
    pub at: i64,
    /// The creating client's working root, canonicalized.
    pub cwd: String,
    /// The origin config's identity: canonicalized explicit `--config` path,
    /// else `<cwd>/efference.toml` when present, else [`BUILTIN_CONFIG_KEY`].
    pub config_key: String,
    pub connection: String,
    pub provider: String,
    pub model: String,
}

/// One explicit separate-triage pick and the main model it ran with. Written
/// beside the main [`Pick`] at creation; nothing reads it yet (docs/state-store.md).
#[derive(Clone, Debug, PartialEq)]
pub struct TriagePick {
    /// Unix seconds when the session was created.
    pub at: i64,
    pub cwd: String,
    pub config_key: String,
    pub main_connection: String,
    pub main_model: String,
    pub triage_connection: String,
    pub triage_model: String,
}

impl TriagePick {
    /// The row for a creation, when it has one: triage was picked explicitly
    /// (`explicit`) and resolved to a separate client (`triage` is `Some`). The
    /// shared-with-main fallback names no triage model and records nothing.
    pub fn for_creation(
        at: i64,
        scope: &Scope,
        explicit: bool,
        main: &crate::user_config::ModelSelection,
        triage: Option<&crate::user_config::ModelSelection>,
    ) -> Option<Self> {
        let triage = triage.filter(|_| explicit)?;
        Some(Self {
            at,
            cwd: scope.cwd.to_string_lossy().into_owned(),
            config_key: scope.config_key.clone(),
            main_connection: main.connection.clone(),
            main_model: main.model.clone(),
            triage_connection: triage.connection.clone(),
            triage_model: triage.model.clone(),
        })
    }
}

/// A decayed-weight model candidate: what to offer, and how strongly.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub connection: String,
    pub model: String,
    /// Sum of decayed weights of the rows behind this candidate.
    pub mass: f64,
}

/// The project scope history keys on: a working root plus the config those
/// sessions were created under. Two configs in one directory accumulate
/// separate model patterns, as they should.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub cwd: PathBuf,
    pub config_key: String,
}

impl Scope {
    /// Derive the scope for a creation request: the canonicalized working
    /// root, and the origin config's identity. The rule mirrors
    /// [`load_in`](crate::config::load_in)'s probe — explicit `--config`,
    /// else `efference.toml` when present, else the builtin sentinel — so the
    /// key names the template boot consumed, not the per-session snapshot
    /// (whose path is unique per session and useless as a history key). A root
    /// that will not canonicalize keeps its absolute spelling rather than
    /// losing the pick.
    pub fn for_request(cwd: &Path, config: Option<&Path>) -> Self {
        let cwd = canonicalize(cwd);
        let config_key = match config {
            Some(explicit) => canonicalize(&cwd.join(explicit))
                .to_string_lossy()
                .into_owned(),
            None => {
                let probed = cwd.join(crate::config::DEFAULT_PATH);
                if probed.is_file() {
                    canonicalize(&probed).to_string_lossy().into_owned()
                } else {
                    BUILTIN_CONFIG_KEY.to_string()
                }
            }
        };
        Self { cwd, config_key }
    }

    fn matches(&self, cwd: &str, config_key: &str) -> bool {
        self.cwd.to_string_lossy() == cwd && self.config_key == config_key
    }
}

fn canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The history store: a path to `state.db`, opened per operation. A store with
/// no path is disabled — what
/// [`InferenceRegistry::from_config`](crate::inference::InferenceRegistry::from_config)
/// carries, since a config value alone names no home directory.
#[derive(Clone, Debug, Default)]
pub struct History {
    path: Option<PathBuf>,
}

impl History {
    /// Open the store at `path`, creating the database and its schema on first
    /// use. `CREATE TABLE IF NOT EXISTS` is the migration story: a later table
    /// lands beside this one, and a later column lands in a follow-up change.
    pub fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// The user state database under `home` (`~/.efference/state.db`).
    pub fn user(home: &Path) -> Self {
        Self::at(crate::platform::state::path_in(home))
    }

    /// No store: every read is empty, every write is a no-op.
    pub fn disabled() -> Self {
        Self { path: None }
    }

    /// The database file, when the store has one. Other tables of the user
    /// state (`src/catalog.rs`) open the same file.
    pub fn state_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn connect(&self) -> Option<rusqlite::Connection> {
        crate::platform::state::open(self.path.as_ref()?, SCHEMA).ok()
    }

    /// Record one explicit pick. Best-effort: a store that cannot be written
    /// must never fail the creation it remembers.
    pub fn record(&self, pick: &Pick) {
        let Some(connection) = self.connect() else {
            return;
        };
        connection
            .execute(
                "INSERT INTO model_picks (at, cwd, config_key, connection, provider, model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    pick.at,
                    pick.cwd,
                    pick.config_key,
                    pick.connection,
                    pick.provider,
                    pick.model,
                ],
            )
            .ok();
    }

    /// Record one explicit separate-triage pick with its main model. Only
    /// explicit picks are written, never the shared-with-main fallback, so the
    /// fallback cannot reinforce itself. Best-effort like [`Self::record`].
    pub fn record_triage(&self, pick: &TriagePick) {
        let Some(connection) = self.connect() else {
            return;
        };
        connection
            .execute(
                "INSERT INTO triage_picks (at, cwd, config_key, main_connection, main_model,
                                           triage_connection, triage_model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    pick.at,
                    pick.cwd,
                    pick.config_key,
                    pick.main_connection,
                    pick.main_model,
                    pick.triage_connection,
                    pick.triage_model,
                ],
            )
            .ok();
    }

    /// The recorded triage picks, oldest first. Tests read it; no production
    /// reader exists yet.
    #[cfg(test)]
    fn triage_picks(&self) -> Vec<TriagePick> {
        let Some(connection) = self.connect() else {
            return Vec::new();
        };
        let mut statement = connection
            .prepare(
                "SELECT at, cwd, config_key, main_connection, main_model,
                        triage_connection, triage_model FROM triage_picks ORDER BY rowid",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(TriagePick {
                    at: row.get(0)?,
                    cwd: row.get(1)?,
                    config_key: row.get(2)?,
                    main_connection: row.get(3)?,
                    main_model: row.get(4)?,
                    triage_connection: row.get(5)?,
                    triage_model: row.get(6)?,
                })
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    }

    fn picks(&self) -> Vec<Pick> {
        let Some(connection) = self.connect() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection
            .prepare("SELECT at, cwd, config_key, connection, provider, model FROM model_picks")
        else {
            return Vec::new();
        };
        statement
            .query_map([], |row| {
                Ok(Pick {
                    at: row.get(0)?,
                    cwd: row.get(1)?,
                    config_key: row.get(2)?,
                    connection: row.get(3)?,
                    provider: row.get(4)?,
                    model: row.get(5)?,
                })
            })
            .map_or_default(|rows| rows.filter_map(Result::ok).collect())
    }

    /// The picks across all connections, most decayed weight first with project
    /// picks before aggregate ones — the list the connection picker shows, so
    /// one menu covers every provider without opening each connection's own
    /// recent menu first. Capped like [`Self::recent`]: memory, not the domain.
    pub fn recent_all(&self, scope: &Scope, now: i64, limit: usize) -> Vec<Candidate> {
        let picks = self.picks();
        let mut candidates = aggregate(picks.iter().collect(), None, now);
        candidates.sort_by(|a, b| {
            let a_project = scope.matches(&a.cwd, &a.config_key);
            let b_project = scope.matches(&b.cwd, &b.config_key);
            b_project.cmp(&a_project).then(
                b.mass
                    .partial_cmp(&a.mass)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        candidates
            .into_iter()
            .map(|ranked| Candidate {
                connection: ranked.connection,
                model: ranked.model,
                mass: ranked.mass,
            })
            .take(limit)
            .collect()
    }

    /// The models picked on `connection`, most decayed weight first, project
    /// picks before aggregate ones. The typed-ID escape hatch stays in every
    /// picker, so the menu is capped: it offers memory, not the domain.
    pub fn recent(
        &self,
        scope: &Scope,
        connection: &str,
        now: i64,
        limit: usize,
    ) -> Vec<Candidate> {
        let picks = self.picks();
        let mut candidates = aggregate(picks.iter().collect(), Some(connection), now);
        candidates.sort_by(|a, b| {
            let a_project = scope.matches(&a.cwd, &a.config_key);
            let b_project = scope.matches(&b.cwd, &b.config_key);
            b_project.cmp(&a_project).then(
                b.mass
                    .partial_cmp(&a.mass)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        candidates
            .into_iter()
            .map(|ranked| Candidate {
                connection: ranked.connection,
                model: ranked.model,
                mass: ranked.mass,
            })
            .take(limit)
            .collect()
    }

    /// The hierarchical default for `scope`: the project mode past
    /// [`PROJECT_MASS_THRESHOLD`], else the system-wide aggregate, else
    /// nothing (docs/transport.md).
    pub fn default(&self, scope: &Scope, now: i64) -> Option<Candidate> {
        let picks = self.picks();
        let project: Vec<&Pick> = picks
            .iter()
            .filter(|pick| scope.matches(&pick.cwd, &pick.config_key))
            .collect();
        let mass: f64 = project.iter().map(|pick| weight(pick.at, now)).sum();
        if mass >= PROJECT_MASS_THRESHOLD
            && let Some(top) = aggregate(project, None, now).into_iter().next()
        {
            return Some(Candidate {
                connection: top.connection,
                model: top.model,
                mass,
            });
        }
        aggregate(picks.iter().collect(), None, now)
            .into_iter()
            .next()
            .map(|ranked| Candidate {
                connection: ranked.connection,
                model: ranked.model,
                mass: ranked.mass,
            })
    }
}

struct Ranked {
    connection: String,
    model: String,
    cwd: String,
    config_key: String,
    mass: f64,
}

/// Fold rows into per-(connection, model) decayed masses, heaviest first.
/// `connection` narrows to one connection's rows; `None` ranks everything.
fn aggregate(picks: Vec<&Pick>, connection: Option<&str>, now: i64) -> Vec<Ranked> {
    let mut masses: std::collections::BTreeMap<(&str, &str), (f64, &Pick)> =
        std::collections::BTreeMap::new();
    for pick in picks {
        if connection.is_some_and(|connection| pick.connection != connection) {
            continue;
        }
        let entry = masses
            .entry((pick.connection.as_str(), pick.model.as_str()))
            .or_insert((0.0, pick));
        entry.0 += weight(pick.at, now);
    }
    let mut ranked: Vec<Ranked> = masses
        .into_values()
        .map(|(mass, pick)| Ranked {
            connection: pick.connection.clone(),
            model: pick.model.clone(),
            cwd: pick.cwd.clone(),
            config_key: pick.config_key.clone(),
            mass,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.mass
            .partial_cmp(&a.mass)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

/// One row's vote at `now`: exponential decay with half-life
/// [`HALF_LIFE_DAYS`]. A pick from the future (clock skew across machines
/// sharing a home directory) counts full rather than overflowing.
fn weight(at: i64, now: i64) -> f64 {
    let age_days = ((now - at).max(0) as f64) / 86_400.0;
    0.5f64.powf(age_days / HALF_LIFE_DAYS)
}

/// Unix seconds now, for stamping picks.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod test;
