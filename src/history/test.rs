//! The history store's contract: decay math, project scoping, and the
//! hierarchical default — against a real database in a temporary directory, so
//! the SQL and the ranking are tested together.

use super::*;
use std::collections::HashMap;

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "efference-history-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn history() -> (History, PathBuf) {
    let dir = dir();
    let store = History::at(dir.join("state.db"));
    (store, dir)
}

fn scope(dir: &Path) -> Scope {
    Scope {
        cwd: canonicalize(dir),
        config_key: BUILTIN_CONFIG_KEY.to_string(),
    }
}

fn pick(at: i64, scope: &Scope, connection: &str, model: &str) -> Pick {
    Pick {
        at,
        cwd: scope.cwd.to_string_lossy().into_owned(),
        config_key: scope.config_key.clone(),
        connection: connection.to_string(),
        provider: "anthropic".to_string(),
        model: model.to_string(),
    }
}

/// Decay halves the weight every [`HALF_LIFE_DAYS`].
#[test]
fn weight_halves_each_half_life() {
    let now = 1_800_000_000;
    let half_life = (HALF_LIFE_DAYS * 86_400.0) as i64;
    assert_eq!(weight(now, now), 1.0);
    assert!((weight(now - half_life, now) - 0.5).abs() < 1e-9);
    assert!((weight(now - 2 * half_life, now) - 0.25).abs() < 1e-9);
}

/// A pick from the future counts full rather than overflowing.
#[test]
fn future_pick_counts_full() {
    assert_eq!(weight(1_800_000_100, 1_800_000_000), 1.0);
}

/// An empty store defaults to nothing: the configured default applies.
#[test]
fn empty_store_has_no_default() {
    let (store, _dir) = history();
    let scope = scope(&PathBuf::from("/code/example"));
    assert_eq!(store.default(&scope, 1_800_000_000), None);
}

/// Below threshold, one stray pick elsewhere does not become a project mode —
/// but it is the aggregate, so it is still the default when nothing stronger
/// speaks.
#[test]
fn single_pick_is_the_aggregate_default() {
    let (store, dir) = history();
    let scope = scope(&dir);
    let now = 1_800_000_000;
    store.record(&pick(now, &scope, "meta", "muse-spark"));
    let candidate = store.default(&scope, now).expect("one pick is an aggregate");
    assert_eq!(candidate.connection, "meta");
    assert_eq!(candidate.model, "muse-spark");
}

/// At threshold mass the project mode wins over a heavier aggregate: the
/// project's own picks outrank the system's.
#[test]
fn project_mass_beats_heavier_aggregate() {
    let (store, dir) = history();
    let project = scope(&dir);
    let elsewhere = Scope {
        cwd: PathBuf::from("/code/other"),
        config_key: BUILTIN_CONFIG_KEY.to_string(),
    };
    let now = 1_800_000_000;
    for _ in 0..6 {
        store.record(&pick(now, &project, "meta", "muse-spark"));
    }
    for _ in 0..20 {
        store.record(&pick(now, &elsewhere, "work", "sonnet"));
    }
    let candidate = store.default(&project, now).expect("project has mass");
    assert_eq!(candidate.connection, "meta");
    assert_eq!(candidate.model, "muse-spark");
}

/// The project mode is the project's top model, not merely its mass.
#[test]
fn project_mode_picks_the_project_top() {
    let (store, dir) = history();
    let scope = scope(&dir);
    let now = 1_800_000_000;
    for _ in 0..4 {
        store.record(&pick(now, &scope, "meta", "muse-spark"));
    }
    for _ in 0..3 {
        store.record(&pick(now, &scope, "meta", "other-model"));
    }
    let candidate = store.default(&scope, now).expect("project has mass");
    assert_eq!(candidate.model, "muse-spark");
}

/// Old picks decay out of the project mode: six picks two half-lives ago weigh
/// ~1.5, below threshold, so the aggregate (still the same model here) answers
/// with its own mass.
#[test]
fn decayed_project_mass_falls_back() {
    let (store, dir) = history();
    let scope = scope(&dir);
    let now = 1_800_000_000;
    let old = now - 2 * (HALF_LIFE_DAYS * 86_400.0) as i64;
    for _ in 0..6 {
        store.record(&pick(old, &scope, "meta", "muse-spark"));
    }
    let candidate = store.default(&scope, now).expect("aggregate remains");
    assert_eq!(candidate.model, "muse-spark");
    assert!(candidate.mass < PROJECT_MASS_THRESHOLD);
}

/// Recent is scoped to the connection and orders by decayed weight.
#[test]
fn recent_scopes_to_connection_by_weight() {
    let (store, dir) = history();
    let scope = scope(&dir);
    let now = 1_800_000_000;
    let old = now - (HALF_LIFE_DAYS * 86_400.0) as i64;
    store.record(&pick(old, &scope, "meta", "stale-model"));
    store.record(&pick(now, &scope, "meta", "fresh-model"));
    store.record(&pick(now, &scope, "work", "other-connection"));
    let models: Vec<String> = store
        .recent(&scope, "meta", now, 10)
        .into_iter()
        .map(|candidate| candidate.model)
        .collect();
    assert_eq!(models, vec!["fresh-model", "stale-model"]);
}

/// The menu cap holds: memory, not the domain.
#[test]
fn recent_respects_limit() {
    let (store, dir) = history();
    let scope = scope(&dir);
    let now = 1_800_000_000;
    for index in 0..5 {
        store.record(&pick(now, &scope, "meta", &format!("model-{index}")));
    }
    assert_eq!(store.recent(&scope, "meta", now, 3).len(), 3);
}

/// A disabled store reads empty and writes nothing.
#[test]
fn disabled_store_is_empty() {
    let store = History::disabled();
    let scope = scope(&PathBuf::from("/code/example"));
    let now = 1_800_000_000;
    store.record(&pick(now, &scope, "meta", "muse-spark"));
    assert_eq!(store.default(&scope, now), None);
    assert!(store.recent(&scope, "meta", now, 10).is_empty());
}

/// Two configs in one directory accumulate separate patterns: the supervisor
/// file's picks do not elect the default checkout's mode.
#[test]
fn config_key_separates_patterns() {
    let (store, dir) = history();
    let now = 1_800_000_000;
    let cwd = canonicalize(&dir);
    let checkout = Scope {
        cwd: cwd.clone(),
        config_key: cwd
            .join(crate::config::DEFAULT_PATH)
            .to_string_lossy()
            .into_owned(),
    };
    let supervisor = Scope {
        cwd,
        config_key: "/code/example/supervisor.toml".to_string(),
    };
    for _ in 0..6 {
        store.record(&pick(now, &supervisor, "work", "sonnet"));
    }
    for _ in 0..6 {
        store.record(&pick(now, &checkout, "meta", "muse-spark"));
    }
    let masses: HashMap<String, f64> = store
        .picks()
        .iter()
        .map(|pick| (pick.config_key.clone(), weight(pick.at, now)))
        .fold(HashMap::new(), |mut masses, (key, weight)| {
            *masses.entry(key).or_insert(0.0) += weight;
            masses
        });
    assert_eq!(masses.len(), 2);
    assert_eq!(
        store.default(&checkout, now).expect("checkout has mass").model,
        "muse-spark"
    );
    assert_eq!(
        store.default(&supervisor, now).expect("supervisor has mass").model,
        "sonnet"
    );
}

/// `for_request` mirrors `load_in`'s probe: explicit path, else the checkout's
/// `efference.toml` when present, else the builtin sentinel.
#[test]
fn scope_for_request_mirrors_load_probe() {
    let dir = dir();
    let cwd = canonicalize(&dir);
    let unprobed = Scope::for_request(&dir, None);
    assert_eq!(unprobed.cwd, cwd);
    assert_eq!(unprobed.config_key, BUILTIN_CONFIG_KEY);
    std::fs::write(dir.join(crate::config::DEFAULT_PATH), "scope = \"x\"\n").unwrap();
    let probed = Scope::for_request(&dir, None);
    assert_eq!(
        probed.config_key,
        canonicalize(&dir.join(crate::config::DEFAULT_PATH))
            .to_string_lossy()
    );
    let explicit = Scope::for_request(&dir, Some(Path::new("custom.toml")));
    assert_eq!(
        explicit.config_key,
        cwd.join("custom.toml").to_string_lossy()
    );
}
