use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn dir() -> tempfile::TempDir {
    crate::testing::temp_dir("catalog")
}

fn rows(ids: &[&str]) -> Vec<ListedModel> {
    ids.iter().map(|id| ListedModel::new(*id)).collect()
}

fn ids(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .rows
        .iter()
        .flatten()
        .map(|row| row.model.clone())
        .collect()
}

const DAY: Duration = CATALOG_TTL;

#[tokio::test]
async fn a_refresh_stores_rows_and_a_young_copy_is_not_read_again() {
    let catalog = Catalog::new(None);
    let reads = AtomicUsize::new(0);
    let read = || async {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(rows(&["a", "b"]))
    };
    let first = catalog
        .refresh_with("c", Provider::Anthropic, DAY, read)
        .await;
    assert_eq!(ids(&first), ["a", "b"]);
    let second = catalog
        .refresh_with("c", Provider::Anthropic, MAX_AGE_FLOOR, read)
        .await;
    assert_eq!(ids(&second), ["a", "b"]);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_refreshes_share_one_read() {
    let catalog = Catalog::new(None);
    let reads = Arc::new(AtomicUsize::new(0));
    let read = || {
        let reads = reads.clone();
        async move {
            reads.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(rows(&["a"]))
        }
    };
    let (one, two) = tokio::join!(
        catalog.refresh_with("c", Provider::Anthropic, DAY, read),
        catalog.refresh_with("c", Provider::Anthropic, DAY, read),
    );
    assert_eq!(ids(&one), ["a"]);
    assert_eq!(ids(&two), ["a"]);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_read_with_nothing_stored_records_the_error() {
    let catalog = Catalog::new(None);
    let snapshot = catalog
        .refresh_with("c", Provider::Anthropic, DAY, || async {
            Err("GET returned 401".into())
        })
        .await;
    assert_eq!(snapshot.rows, None);
    assert_eq!(snapshot.failure.unwrap().error, "GET returned 401");
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_old_copy_beside_the_error() {
    let dir = dir();
    let path = dir.path().join("state.db");
    let long_ago = crate::history::now() - 3 * DAY.as_secs() as i64;
    store::save(
        &path,
        "c",
        &Stored {
            provider: "anthropic".into(),
            fetched_at: long_ago,
            rows: rows(&["old"]),
        },
    );
    let catalog = Catalog::new(Some(path));
    let snapshot = catalog
        .refresh_with("c", Provider::Anthropic, DAY, || async {
            Err("timed out".into())
        })
        .await;
    assert_eq!(ids(&snapshot), ["old"]);
    assert_eq!(snapshot.failure.as_ref().unwrap().error, "timed out");
    assert!(snapshot.age.unwrap() >= 3 * DAY.as_secs());
    let entry = assemble("c", Provider::Anthropic, snapshot, None, &[]);
    assert_eq!(entry.status, Status::Failed);
    assert_eq!(entry.error.as_deref(), Some("timed out"));
    assert_eq!(entry.models.len(), 1);
}

#[tokio::test]
async fn a_stored_copy_survives_a_new_handle() {
    let dir = dir();
    let path = dir.path().join("state.db");
    Catalog::new(Some(path.clone()))
        .refresh_with("c", Provider::Anthropic, DAY, || async { Ok(rows(&["a"])) })
        .await;
    let snapshot =
        Catalog::new(Some(path)).snapshot("c", Provider::Anthropic, crate::history::now());
    assert_eq!(ids(&snapshot), ["a"]);
    assert!(snapshot.age.unwrap() < 5);
}

#[test]
fn a_copy_from_another_provider_is_discarded() {
    let dir = dir();
    let path = dir.path().join("state.db");
    store::save(
        &path,
        "c",
        &Stored {
            provider: "openai".into(),
            fetched_at: crate::history::now(),
            rows: rows(&["gpt"]),
        },
    );
    let catalog = Catalog::new(Some(path.clone()));
    let snapshot = catalog.snapshot("c", Provider::Anthropic, crate::history::now());
    assert_eq!(snapshot.rows, None);
    assert_eq!(store::load(&path, "c"), None);
}

fn held(age: u64, failure: Option<&str>, ids: &[&str]) -> Snapshot {
    Snapshot {
        rows: Some(rows(ids)),
        age: Some(age),
        failure: failure.map(|error| Failure {
            at: 0,
            error: error.into(),
        }),
    }
}

#[test]
fn status_follows_age_failure_and_availability() {
    let status = |snapshot, unavailable: Option<&str>| {
        assemble("c", Provider::Anthropic, snapshot, unavailable, &[]).status
    };
    let day = DAY.as_secs();
    assert_eq!(status(held(10, None, &["a"]), None), Status::Fresh);
    assert_eq!(status(held(day, None, &["a"]), None), Status::Stale);
    assert_eq!(status(held(day, Some("x"), &["a"]), None), Status::Failed);
    // A failed extra read inside the TTL leaves the copy fresh and reports why.
    let fresh = assemble(
        "c",
        Provider::Anthropic,
        held(10, Some("x"), &["a"]),
        None,
        &[],
    );
    assert_eq!(
        (fresh.status, fresh.error.as_deref()),
        (Status::Fresh, Some("x"))
    );
    let none = Snapshot {
        rows: None,
        age: None,
        failure: None,
    };
    assert_eq!(status(none.clone(), None), Status::Missing);
    assert_eq!(status(none, Some("no key")), Status::Unavailable);
}

#[test]
fn recent_ranks_fold_onto_rows_and_unlisted_picks_become_bare_rows() {
    let recents = [
        Recent {
            model: "b".into(),
            rank: 1,
        },
        Recent {
            model: "anthropic/typed-by-hand".into(),
            rank: 2,
        },
    ];
    let entry = assemble(
        "or",
        Provider::OpenRouter,
        held(10, None, &["a", "b"]),
        None,
        &recents,
    );
    assert_eq!(
        serde_json::to_string(&entry).unwrap(),
        concat!(
            r#"{"connection":"or","provider":"openrouter","status":"fresh","age":10,"models":["#,
            r#"{"connection":"or","model":"a"},"#,
            r#"{"connection":"or","model":"b","recent":1},"#,
            r#"{"connection":"or","model":"anthropic/typed-by-hand","group":"anthropic","recent":2}]}"#
        )
    );
    let back: ConnectionCatalog =
        serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
    assert_eq!(back, entry);
}

#[tokio::test]
async fn descriptions_survive_storage_and_are_flattened_into_route_rows() {
    let dir = dir();
    let path = dir.path().join("state.db");
    let row = ListedModel::new("a").described(Some("Provider prose."));
    Catalog::new(Some(path.clone()))
        .refresh_with("c", Provider::OpenRouter, DAY, || async { Ok(vec![row]) })
        .await;
    let snapshot =
        Catalog::new(Some(path)).snapshot("c", Provider::OpenRouter, crate::history::now());
    let entry = assemble("c", Provider::OpenRouter, snapshot, None, &[]);
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["models"][0]["description"], "Provider prose.");
    assert_eq!(json["models"][0]["connection"], "c");
}

/// The rule for which models a role may be offered: turn roles take text rows
/// only, and a provider that serves no turns has none to give them.
#[test]
fn a_role_is_offered_only_the_models_it_can_run() {
    use crate::api::ModelKind;
    let decision = |id: &str| ListedModel {
        kind: ModelKind::Decisions,
        ..ListedModel::new(id)
    };
    let snapshot = Snapshot {
        rows: Some(vec![
            ListedModel::new("anthropic/claude-haiku"),
            decision("upstage/solar-decide"),
        ]),
        age: Some(0),
        failure: None,
    };
    let entry = assemble("or", Provider::OpenRouter, snapshot, None, &[]);
    let models = |role: ModelRole| -> Vec<String> {
        entry
            .clone()
            .for_role(role)
            .unwrap()
            .models
            .into_iter()
            .map(|m| m.row.model)
            .collect()
    };
    assert_eq!(models(ModelRole::Main), ["anthropic/claude-haiku"]);
    assert_eq!(models(ModelRole::Triage), ["anthropic/claude-haiku"]);
    assert_eq!(
        models(ModelRole::Spotter),
        ["anthropic/claude-haiku", "upstage/solar-decide"]
    );

    let typesafe = assemble(
        "ts",
        Provider::TypeSafe,
        Snapshot {
            rows: None,
            age: None,
            failure: None,
        },
        None,
        &[Recent {
            model: "jev-latest".into(),
            rank: 1,
        }],
    );
    // A pick the listing does not name inherits the connection's kind.
    assert_eq!(typesafe.models[0].row.kind, ModelKind::Decisions);
    assert!(typesafe.clone().for_role(ModelRole::Main).is_none());
    assert_eq!(
        typesafe.for_role(ModelRole::Spotter).unwrap().models.len(),
        1
    );

    assert_eq!(ModelRole::from_name("spotter"), Some(ModelRole::Spotter));
    assert_eq!(ModelRole::from_name("judge"), None);
}
