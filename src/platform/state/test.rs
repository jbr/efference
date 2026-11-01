//! The shared open: the directory arrives, each table's schema lands beside the
//! others', and a file that is not a database is an error the caller can see.

use super::*;
use std::time::SystemTime;

/// Two scratch tables standing in for the real ones, which is the property the
/// shared open exists for: they take turns creating the same file, and neither
/// one's schema disturbs the other's rows.
const PICKS: &str = "CREATE TABLE IF NOT EXISTS picks (connection TEXT PRIMARY KEY);";
const COUNTS: &str = "CREATE TABLE IF NOT EXISTS counts (n INTEGER PRIMARY KEY);";

fn dir() -> tempfile::TempDir {
    crate::testing::temp_dir("state")
}

#[test]
fn each_tables_schema_lands_in_the_one_file() {
    let dir = dir();
    let path = dir.path().join("nested/state.db");

    open(&path, PICKS)
        .unwrap()
        .execute("INSERT INTO picks (connection) VALUES ('anthropic')", [])
        .unwrap();
    open(&path, COUNTS)
        .unwrap()
        .execute_batch(
            "INSERT INTO counts (n) VALUES (1000);
             INSERT INTO counts (n) VALUES (1001);",
        )
        .unwrap();

    // Re-opening with either schema again must not disturb what the other wrote.
    open(&path, PICKS).unwrap();
    let counts: i64 = open(&path, COUNTS)
        .unwrap()
        .query_row("SELECT count(*) FROM counts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(counts, 2);
    let picks: String = open(&path, PICKS)
        .unwrap()
        .query_row("SELECT connection FROM picks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(picks, "anthropic");
}

#[test]
fn the_parent_directory_is_created() {
    let dir = dir();
    let path = dir.path().join("made/on/demand/state.db");
    open(&path, PICKS).unwrap();
    assert!(path.exists());
}

/// The production topology: the push store, the enrolled devices, and the pending
/// sign-ins name the same file, and none must be able to damage another's rows.
/// This is the case `open` exists for, so it is asserted on the real stores rather
/// than on stand-in tables.
#[test]
fn the_real_stores_coexist_in_one_user_scoped_file() {
    let dir = dir();
    let path = dir.path().join("state.db");
    let store = crate::platform::push::Store::new(path.clone());
    let sign_ins = crate::platform::sign_ins::Store::new(path.clone());
    let devices = crate::platform::devices::Store::new(path.clone());
    let device = devices
        .insert(
            crate::platform::devices::Kind::Browser,
            "phone",
            crate::platform::devices::Joined::Code,
            SystemTime::now(),
        )
        .unwrap();

    store
        .upsert(&crate::platform::push::Subscription {
            endpoint: "https://push.example.net/a".into(),
            p256dh: "p".into(),
            auth: "a".into(),
        })
        .unwrap();
    let requested = sign_ins.request("tablet", SystemTime::now()).unwrap();

    // Each store opening the file applies only its own schema.
    assert_eq!(store.load().len(), 1);
    assert_eq!(sign_ins.waiting(SystemTime::now()).unwrap().len(), 1);
    assert_eq!(store.load()[0].endpoint, "https://push.example.net/a");
    assert_eq!(devices.list().unwrap(), [device]);
    assert!(
        sign_ins
            .approve(&requested.code, SystemTime::now())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    // A best-effort caller folds this away and reads as empty; the web bind's
    // tables report it and refuse the request. Either way it must be an error
    // rather than a connection that silently reports no tables.
    let dir = dir();
    let path = dir.path().join("state.db");
    std::fs::write(&path, "{ not a database").unwrap();
    assert!(matches!(open(&path, PICKS), Err(Error::Sqlite(_))));
}
