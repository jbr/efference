//! `GET /projects` and `DELETE /projects`: the remembered launch locations
//! (docs/control-api.md).
//!
//! What is **not** here is a successful `POST /sessions` recorded end to end.
//! `boot::prepare` makes the provider's capability lookup a required step and a
//! failed one fails the creation, so a creation that reaches the record has
//! already reached a provider — and the repo has no stand-in provider to reach.
//! The write itself is [`crate::remembered`]'s to test, and the refusal below
//! covers the other half of the placement: a creation that never started
//! remembers nothing.

use super::*;

/// A host whose state database — picks, catalog and remembered locations alike —
/// is a file under this test's root.
fn host_with_store(roots: &Roots) -> Arc<Host> {
    Host::new(
        one_connection_registry()
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        baseline_env(),
    )
}

/// The store the host above writes through, opened on the same file. Seeding
/// through it rather than through the host keeps the assertions about what a
/// launch leaves behind separate from what the routes report.
fn store(roots: &Roots) -> crate::remembered::Projects {
    crate::remembered::Projects::at(roots.0.join("state.db"))
}

/// A row as a launch in `cwd` recorded it, optionally naming a configuration.
fn launch(cwd: &Path, config: Option<&str>, at: i64) -> crate::remembered::Project {
    crate::remembered::Project {
        last_used_at: at,
        cwd: cwd.to_path_buf(),
        config_key: match config {
            Some(config) => cwd.join(config).to_string_lossy().into_owned(),
            None => crate::history::BUILTIN_CONFIG_KEY.to_string(),
        },
        config: config.map(PathBuf::from),
    }
}

/// The number of entries in an index body, so a filter is asserted on the count
/// and the shape separately.
fn count(body: &str) -> usize {
    json(body)["projects"].as_array().map_or(0, Vec::len)
}

/// Percent-encode a query value. A `cwd` carries a `/` and `Roots` puts one
/// under `/var/folders/…`, where a space and a digit-run are common; a query that
/// is not encoded names a different path than the one stored.
fn query_value(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// The forget route for one identity, as the index reports it.
fn forget_url(cwd: &Path, config_key: &str) -> String {
    format!(
        "/projects?cwd={}&config_key={}",
        query_value(&cwd.to_string_lossy()),
        query_value(config_key)
    )
}

/// The index reports each remembered location in the shape a picker creates
/// from, most recently used first, with the label derived rather than stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_index_reports_launches_newest_first_with_a_derived_label() {
    let roots = Roots::new();
    let host = host_with_store(&roots);
    let server = host_server(&host).await;

    // Stamped out of write order: recency is the column, not the write.
    let store = store(&roots);
    store.record(&launch(&roots.0.join("one"), None, 200));
    store.record(&launch(&roots.0.join("two"), None, 300));
    store.record(&launch(&roots.0.join("one"), Some("eff.toml"), 100));
    std::fs::write(roots.0.join("one/eff.toml"), "").unwrap();

    let body = server.get("/projects").await;
    body.assert_status(200);
    let rows = json(body.body())["projects"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 3, "{}", body.body());
    assert_eq!(
        rows[0]["cwd"].as_str().unwrap(),
        roots.0.join("two").to_string_lossy()
    );
    assert_eq!(rows[0]["label"], "two");
    assert_eq!(rows[0]["last_used_at"], 300);
    assert_eq!(rows[0]["config"], Value::Null, "nothing was named");

    // The configuration distinction is in the row, so one directory can appear
    // twice without the picker offering two identical entries.
    assert_eq!(rows[2]["label"], "one");
    assert_eq!(rows[1]["cwd"], rows[2]["cwd"], "one directory");
    assert_eq!(rows[2]["config"], "eff.toml");
    assert_ne!(rows[1]["config_key"], rows[2]["config_key"]);

    host.swansong().shut_down().await;
}

/// The index filters against the filesystem and writes nothing: an entry whose
/// working root is gone is omitted, and it is on the next answer once the volume
/// is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_location_is_omitted_but_still_remembered() {
    let roots = Roots::new();
    let host = host_with_store(&roots);
    let server = host_server(&host).await;
    let store = store(&roots);

    let volume = launch(&roots.0.join("volume"), None, 100);
    let misconfigured = launch(&roots.0.join("two"), Some("eff.toml"), 300);
    store.record(&launch(&roots.0.join("one"), None, 200));
    store.record(&volume);
    store.record(&misconfigured);
    // Each is launchable when recorded: the volume is a directory, and the
    // configuration it names is a file.
    std::fs::create_dir_all(roots.0.join("volume")).unwrap();
    std::fs::write(roots.0.join("two/eff.toml"), "").unwrap();
    assert_eq!(count(server.get("/projects").await.body()), 3);

    // A volume that is not mounted, and a named configuration that is gone: both
    // are refusals creation would give, so the index offers neither.
    std::fs::remove_dir_all(roots.0.join("volume")).unwrap();
    assert_eq!(count(server.get("/projects").await.body()), 2);
    let body = server.get("/projects").await;
    assert!(
        !json(body.body())["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["cwd"].as_str()
                == Some(roots.0.join("volume").to_string_lossy().as_ref())),
        "the unmounted volume is not offered: {}",
        body.body()
    );
    assert!(
        store.contains(&roots.0.join("volume"), &volume.config_key),
        "and it is still remembered, for when the volume returns"
    );

    std::fs::create_dir_all(roots.0.join("volume")).unwrap();
    assert_eq!(
        count(server.get("/projects").await.body()),
        3,
        "and it is offered again"
    );

    // The named configuration is what creation would refuse on, so its absence
    // takes the entry out of the index.
    std::fs::remove_file(roots.0.join("two/eff.toml")).unwrap();
    assert_eq!(
        count(server.get("/projects").await.body()),
        2,
        "a configuration that was named must be there"
    );

    // The probed one is not: creation falls back to its defaults where a
    // configuration was named, so a directory that loses the `efference.toml` it
    // had is still launchable, and stays listed with the probe left to run again.
    std::fs::write(roots.0.join("one/efference.toml"), "").unwrap();
    assert_eq!(count(server.get("/projects").await.body()), 2);
    std::fs::remove_file(roots.0.join("one/efference.toml")).unwrap();
    let body = server.get("/projects").await;
    assert_eq!(
        count(body.body()),
        2,
        "the probed configuration is not a requirement: {}",
        body.body()
    );
    let rows = json(body.body());
    let one = rows["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["cwd"] == roots.0.join("one").to_string_lossy().as_ref())
        .expect("the directory is still offered");
    assert_eq!(
        one["config"],
        Value::Null,
        "and is offered as one that names no configuration"
    );

    host.swansong().shut_down().await;
}

/// Forgetting removes the remembered entry and touches nothing else: not the
/// directory, not a file in it, not the same directory's other configuration, and
/// not a session that ran there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgetting_deletes_the_entry_and_nothing_else() {
    let roots = Roots::new();
    let host = host_with_store(&roots);
    let server = host_server(&host).await;
    let store = store(&roots);
    host.adopt(
        "one".into(),
        runtime::started(runtime::boot(&roots.0.join("one")), host.swansong()).await,
    )
    .expect("the id is free in this test");

    let file = roots.0.join("one/notes.md");
    std::fs::write(&file, "kept").unwrap();
    let unnamed = launch(&roots.0.join("one"), None, 200);
    let named = launch(&roots.0.join("one"), Some("eff.toml"), 300);
    store.record(&unnamed);
    store.record(&named);
    std::fs::write(roots.0.join("one/eff.toml"), "").unwrap();

    server
        .delete(&forget_url(&roots.0.join("one"), &named.config_key))
        .await
        .assert_status(200);
    assert!(
        !store.contains(&roots.0.join("one"), &named.config_key),
        "the named configuration is forgotten"
    );
    assert!(
        store.contains(&roots.0.join("one"), &unnamed.config_key),
        "the same directory's other identity is not"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "kept");
    assert!(
        roots.0.join("one/eff.toml").exists(),
        "a configuration is a file on disk and an entry in here"
    );
    assert_eq!(
        host.ids(),
        ["one"],
        "and the session that ran there is untouched"
    );

    // An identity that is not remembered, and requests that name no identity.
    server
        .delete(&forget_url(&roots.0.join("one"), &named.config_key))
        .await
        .assert_status(404);
    server.delete("/projects").await.assert_status(400);
    server
        .delete("/projects?cwd=/nowhere")
        .await
        .assert_status(400);

    host.swansong().shut_down().await;
}

/// The identity is taken as the index reported it. Recomputing it would re-probe
/// the filesystem, and a directory that grew an `efference.toml` since its launch
/// resolves to a different key — which would leave the entry on screen and answer
/// `404` to the forget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgetting_uses_the_reported_identity_rather_than_recomputing_it() {
    let roots = Roots::new();
    let host = host_with_store(&roots);
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");

    // Launched with nothing named, so the probe's answer — the built-in default —
    // is the identity.
    let row = launch(&cwd, None, 100);
    store(&roots).record(&row);
    assert_eq!(row.config_key, crate::history::BUILTIN_CONFIG_KEY);

    // The directory gains a template afterwards, so re-probing resolves to a key
    // this row was never stored under.
    std::fs::write(cwd.join("efference.toml"), "").unwrap();
    assert_eq!(
        crate::history::Scope::for_request(&cwd, None).config_key,
        cwd.join("efference.toml").to_string_lossy(),
        "the probe finds it now"
    );

    let listed = json(server.get("/projects").await.body());
    let reported = listed["projects"][0]["config_key"].as_str().unwrap();
    assert_eq!(
        reported, row.config_key,
        "the index reports the identity the entry is stored under"
    );
    assert_eq!(
        listed["projects"][0]["config"],
        Value::Null,
        "and the spelling to create with, which is to name nothing"
    );

    server
        .delete(&forget_url(&cwd, reported))
        .await
        .assert_status(200);
    assert_eq!(server.get("/projects").await.body(), r#"{"projects":[]}"#);

    host.swansong().shut_down().await;
}

/// Recording is placed after the session starts, so a creation that never got
/// that far remembers nothing — and, like every other refusal, mints nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creation_that_does_not_start_remembers_nothing() {
    let roots = Roots::new();
    let host = host_with_store(&roots);
    let server = host_server(&host).await;

    let state = created_state(&server, &stops_at_boot(&roots.0.join("one"))).await;
    assert!(!failure_reason(&state).is_empty(), "{state}");

    assert!(
        store(&roots).recent().is_empty(),
        "a refused creation is not a launch"
    );
    assert_eq!(
        server.get("/projects").await.body(),
        r#"{"projects":[]}"#,
        "and the index is empty rather than failing"
    );
    assert!(host.ids().is_empty());

    host.swansong().shut_down().await;
}
