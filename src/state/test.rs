//! The shared open: the directory arrives, each table's schema lands beside the
//! others', and a file that is not a database is an error the caller can see.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// Two scratch tables standing in for the real ones, which is the property the
/// shared open exists for: they take turns creating the same file, and neither
/// one's schema disturbs the other's rows.
const PICKS: &str = "CREATE TABLE IF NOT EXISTS picks (connection TEXT PRIMARY KEY);";
const BURNS: &str = "CREATE TABLE IF NOT EXISTS burns (step INTEGER PRIMARY KEY);";

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "efference-state-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn each_tables_schema_lands_in_the_one_file() {
    let path = dir().join("nested/state.db");

    open(&path, PICKS)
        .unwrap()
        .execute("INSERT INTO picks (connection) VALUES ('anthropic')", [])
        .unwrap();
    open(&path, BURNS)
        .unwrap()
        .execute_batch(
            "INSERT INTO burns (step) VALUES (1000);
             INSERT INTO burns (step) VALUES (1001);",
        )
        .unwrap();

    // Re-opening with either schema again must not disturb what the other wrote.
    open(&path, PICKS).unwrap();
    let burns: i64 = open(&path, BURNS)
        .unwrap()
        .query_row("SELECT count(*) FROM burns", [], |row| row.get(0))
        .unwrap();
    assert_eq!(burns, 2);
    let picks: String = open(&path, PICKS)
        .unwrap()
        .query_row("SELECT connection FROM picks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(picks, "anthropic");
}

#[test]
fn the_parent_directory_is_created() {
    let path = dir().join("made/on/demand/state.db");
    open(&path, PICKS).unwrap();
    assert!(path.exists());
}

/// The production topology: the push store and the burn ledger name the same
/// file, and one must not be able to damage the other's rows. This is the case
/// `open` exists for, so it is asserted on the real stores rather than on
/// stand-in tables.
#[test]
fn the_real_stores_coexist_in_one_user_scoped_file() {
    let path = dir().join("state.db");
    let store = crate::push::Store::new(path.clone());
    let ledger = crate::auth::BurnLedger::new(path.clone());

    store
        .upsert(crate::push::Subscription {
            endpoint: "https://push.example.net/a".into(),
            p256dh: "p".into(),
            auth: "a".into(),
        })
        .unwrap();
    assert!(ledger.burn(4000).unwrap());

    // Each store opening the file applies only its own schema.
    assert_eq!(store.load().len(), 1);
    assert!(ledger.is_burned(4000));
    assert_eq!(store.load()[0].endpoint, "https://push.example.net/a");
}

#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    // A best-effort caller folds this away and reads as empty; the burn ledger
    // reports it and fails the code. Either way it must be an error rather than a
    // connection that silently reports no tables.
    let path = dir().join("state.db");
    std::fs::write(&path, "{ not a database").unwrap();
    assert!(matches!(open(&path, PICKS), Err(Error::Sqlite(_))));
}
