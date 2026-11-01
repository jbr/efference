//! The remembered-location store's contract: identity and deduplication,
//! recency, the availability filter, and forgetting — against a real database in
//! a temporary directory, so the SQL and the filter are tested together.

use super::*;

fn dir() -> tempfile::TempDir {
    crate::inference::testing::temp_dir("remembered")
}

fn store() -> (Projects, tempfile::TempDir) {
    let dir = dir();
    (Projects::at(dir.path().join("state.db")), dir)
}

/// A row as a launch in `cwd` records it: no named configuration, so the
/// probe's own answer is the identity.
fn launch(cwd: &Path, at: i64) -> Project {
    Project {
        last_used_at: at,
        cwd: cwd.to_path_buf(),
        config_key: crate::config::history::BUILTIN_CONFIG_KEY.to_string(),
        config: None,
    }
}

/// A row for a creation that named its configuration: the identity is that
/// file's own path, and the spelling is kept beside it.
fn named_launch(cwd: &Path, config: &Path, at: i64) -> Project {
    Project {
        last_used_at: at,
        cwd: cwd.to_path_buf(),
        config_key: cwd.join(config).to_string_lossy().into_owned(),
        config: Some(config.to_path_buf()),
    }
}

/// A directory under `dir`, created.
fn project_dir(dir: &Path, name: &str) -> PathBuf {
    let cwd = dir.join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    cwd
}

#[test]
fn one_identity_is_one_row_and_a_relaunch_refreshes_it() {
    let (projects, dir) = store();
    let cwd = project_dir(dir.path(), "one");

    projects.record(&launch(&cwd, 100));
    projects.record(&launch(&cwd, 200));

    let rows = projects.recent();
    assert_eq!(rows.len(), 1, "the same identity is one row");
    assert_eq!(
        rows[0].last_used_at, 200,
        "and the later launch is its time"
    );
}

#[test]
fn two_configurations_in_one_directory_are_two_entries() {
    let (projects, dir) = store();
    let cwd = project_dir(dir.path(), "one");
    std::fs::write(cwd.join("a.toml"), "").unwrap();
    std::fs::write(cwd.join("b.toml"), "").unwrap();

    projects.record(&launch(&cwd, 100));
    projects.record(&named_launch(&cwd, Path::new("a.toml"), 200));
    projects.record(&named_launch(&cwd, Path::new("b.toml"), 300));

    let rows = projects.recent();
    assert_eq!(rows.len(), 3, "a probe and two named configurations differ");
    assert!(
        rows.iter().all(|row| row.cwd == cwd),
        "one directory, three identities: {:?}",
        rows.iter().map(|row| &row.config_key).collect::<Vec<_>>()
    );
}

#[test]
fn entries_read_back_most_recently_used_first() {
    let (projects, dir) = store();
    for (name, at) in [("one", 300), ("two", 100), ("three", 200)] {
        let cwd = project_dir(dir.path(), name);
        // Stamped out of write order on purpose: recency is the column.
        projects.record(&launch(&cwd, at));
    }
    let labels: Vec<String> = projects.recent().iter().map(Project::label).collect();
    assert_eq!(labels, ["one", "three", "two"]);
}

#[test]
fn the_index_is_capped_and_keeps_the_newest() {
    let (projects, dir) = store();
    for index in 0..INDEX_LIMIT + 5 {
        let cwd = project_dir(dir.path(), &format!("dir-{index}"));
        projects.record(&launch(&cwd, index as i64));
    }
    let rows = projects.recent();
    assert_eq!(rows.len(), INDEX_LIMIT);
    assert_eq!(
        rows.first().map(|row| row.last_used_at),
        Some(INDEX_LIMIT as i64 + 4),
        "the cap keeps the most recent, not the first written"
    );
}

#[test]
fn the_configuration_recorded_is_the_template_not_a_session_copy() {
    let (projects, dir) = store();
    let cwd = project_dir(dir.path(), "one");
    let template = Path::new("eff.toml");
    std::fs::write(cwd.join(template), "").unwrap();

    projects.record(&named_launch(&cwd, template, 100));

    let row = &projects.recent()[0];
    assert_eq!(row.config.as_deref(), Some(template), "the spelling kept");
    assert_eq!(
        row.config_key,
        cwd.join(template).to_string_lossy(),
        "the identity is the template's own path"
    );
}

/// The identity is the scope a creation already computed, so this pins that the
/// two agree — including that a named configuration keys on the template rather
/// than on a session's private copy of it.
#[test]
fn the_identity_is_the_scope_creation_computed() {
    let dir = dir();
    let cwd = project_dir(dir.path(), "one");
    let unnamed = crate::config::history::Scope::for_request(&cwd, None);
    assert_eq!(
        unnamed.config_key,
        crate::config::history::BUILTIN_CONFIG_KEY
    );

    let named = Path::new("nested/eff.toml");
    std::fs::create_dir_all(cwd.join("nested")).unwrap();
    std::fs::write(cwd.join(named), "").unwrap();
    let scope = crate::config::history::Scope::for_request(&cwd, Some(named));
    // Canonicalized, so the key survives a symlinked path spelling (`/tmp` on
    // macOS) — which is also why a client is handed `config`, not this key, to
    // create with.
    assert_eq!(
        PathBuf::from(&scope.config_key),
        std::fs::canonicalize(cwd.join(named)).unwrap()
    );
    assert!(
        !scope.config_key.contains(".efference"),
        "never the per-session copy: {}",
        scope.config_key
    );
}

#[test]
fn availability_follows_the_configuration_probe() {
    let dir = dir();
    let present = project_dir(dir.path(), "present");
    let unconfigured = project_dir(dir.path(), "unconfigured");
    let misconfigured = project_dir(dir.path(), "misconfigured");
    std::fs::write(misconfigured.join("eff.toml"), "").unwrap();
    let absent = dir.path().join("absent");

    let rows = [
        launch(&present, 100),
        launch(&unconfigured, 100),
        named_launch(&misconfigured, Path::new("eff.toml"), 100),
    ];
    for row in &rows {
        assert!(row.available(), "{} is launchable", row.label());
    }

    // A working root that is not there, and a file where one should be.
    assert!(!launch(&absent, 100).available(), "no directory, no launch");
    std::fs::write(&absent, "not a directory").unwrap();
    assert!(
        !launch(&absent, 100).available(),
        "a file is not a working root"
    );

    // A named configuration that is gone is an error to creation; an unnamed one
    // is not, because creation falls back to its defaults.
    std::fs::remove_file(misconfigured.join("eff.toml")).unwrap();
    assert!(!rows[2].available(), "a named configuration must be there");
    assert!(
        rows[1].available(),
        "an unnamed configuration is creation's probe, not a requirement"
    );
}

#[test]
fn a_label_is_the_directory_basename() {
    assert_eq!(
        launch(Path::new("/Users/jbr/code/efference"), 1).label(),
        "efference"
    );
    assert_eq!(launch(Path::new("/tmp/"), 1).label(), "tmp");
    assert_eq!(
        launch(Path::new("/"), 1).label(),
        "/",
        "a root has no basename"
    );
}

#[test]
fn a_filtered_entry_is_still_stored() {
    let (projects, dir) = store();
    let cwd = project_dir(dir.path(), "volume");
    let row = launch(&cwd, 100);
    projects.record(&row);
    assert!(row.available());
    assert!(projects.contains(&cwd, &row.config_key));

    // An unmounted volume: the index omits it, and the row stays for the mount.
    std::fs::remove_dir_all(&cwd).unwrap();
    assert!(!row.available(), "omitted from the index");
    assert!(
        projects.contains(&cwd, &row.config_key),
        "still stored: the volume can come back"
    );

    std::fs::create_dir_all(&cwd).unwrap();
    assert!(projects.contains(&cwd, &row.config_key), "and it does");
}

#[test]
fn forgetting_deletes_the_entry_and_nothing_on_disk() {
    let (projects, dir) = store();
    let cwd = project_dir(dir.path(), "one");
    let file = cwd.join("notes.md");
    std::fs::write(&file, "kept").unwrap();
    let row = launch(&cwd, 100);
    let other = named_launch(&cwd, Path::new("eff.toml"), 100);
    projects.record(&row);
    projects.record(&other);

    assert!(projects.forget(&cwd, &row.config_key), "the row was there");
    assert!(!projects.contains(&cwd, &row.config_key), "and is gone");
    assert_eq!(projects.recent().len(), 1, "the other identity remains");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "kept",
        "forgetting a location is not a filesystem operation"
    );

    assert!(
        !projects.forget(&cwd, &row.config_key),
        "forgetting twice reports nothing forgotten"
    );
    assert!(
        !projects.forget(&cwd, "no-such-key"),
        "nor an unknown identity"
    );
    assert!(cwd.is_dir(), "the directory is still there");
}

/// A store whose database cannot be opened folds the error away, which is what
/// keeps an unusable path from failing the creation that was remembering itself.
#[test]
fn an_unusable_database_reads_empty() {
    let dir = dir();
    let unusable = dir.path().join("not-a-database");
    std::fs::create_dir_all(&unusable).unwrap();
    let projects = Projects::at(unusable);
    let row = launch(dir.path(), 100);
    projects.record(&row);
    assert!(projects.recent().is_empty());
    assert!(!projects.contains(dir.path(), &row.config_key));
    assert!(!projects.forget(dir.path(), &row.config_key));
}

#[test]
fn a_disabled_store_is_empty_rather_than_failing() {
    let projects = Projects::disabled();
    let cwd = PathBuf::from("/nowhere/at/all");
    let row = launch(&cwd, 100);
    projects.record(&row);
    assert!(projects.recent().is_empty());
    assert!(!projects.contains(&cwd, &row.config_key));
    assert!(!projects.forget(&cwd, &row.config_key));
}
