//! Archiving a session that made its own git worktree (docs/control-api.md): the
//! response's `worktree` field, the directory's fate, and the log entry. Hosts here
//! boot sessions that never reach a provider; the worktree is made with
//! `crate::host::worktree::add` and the session's directories pointed at it.

use super::*;
use crate::record::log::{Payload, WorktreeEventKind};
use serde_json::Value;

/// Host a session named `id` whose working directory is a worktree it made.
pub(super) async fn hosting_worktree(roots: &Roots, host: &Arc<Host>, id: &str) -> PathBuf {
    let env = crate::config::env_table::ChildEnv::default();
    let added = crate::host::worktree::add(&roots.0, id, &roots.0, &env).unwrap();
    hosting_at(roots, host, id, &added.path).await;
    added.path
}

/// Host a session named `id` that works in `working` — a worktree root or a
/// directory inside one (docs/worktrees.md).
pub(super) async fn hosting_at(roots: &Roots, host: &Arc<Host>, id: &str, working: &Path) {
    let mut boot = runtime::boot(&roots.0.join("one"));
    boot.paths = SessionPaths::new(
        crate::config::policy::Directories::new(roots.0.clone(), working.to_path_buf()),
        roots.0.join(".efference").join(id),
    )
    .unwrap();
    host.adopt(id.into(), runtime::started(boot, host.swansong()).await)
        .expect("the id is free in this test");
}

/// A session on disk as an end leaves it: a turn said, and `archived` when it was
/// archived on purpose (docs/resumption.md). Its log carries the boot preamble a
/// resume reads.
pub(super) async fn dormant(roots: &Roots, id: &str, working: &Path, archived: bool) {
    let mut asked = boot::test::request(&roots.0, id, None);
    asked.directories =
        crate::config::policy::Directories::new(roots.0.clone(), working.to_path_buf());
    let mut boot = boot::prepare(asked, &locks::Locks::default())
        .await
        .expect("the session mints");
    boot.log.append(1, Payload::user("hi")).unwrap();
    if archived {
        boot.log.append(1, Payload::archived(None)).unwrap();
    }
    boot::test::drop_boot(boot);
}

pub(super) fn worktree_events(session: &Session) -> Vec<WorktreeEventKind> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|entry| match entry.payload {
            Payload::Worktree(event) => Some(event.kind),
            _ => None,
        })
        .collect()
}

/// A clean worktree is removed on `DELETE`, with its branch (no commit is unique to
/// it), and the outcome is in the response and the session's log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_session_removes_its_clean_worktree() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "wt-fox").await;
    let session = host.session("wt-fox").unwrap();
    // A session with a turn is archived; an empty one is discarded instead.
    session.append(1, Payload::User(vec![])).unwrap();

    let body = server
        .delete("/sessions/wt-fox")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(
        body,
        r#"{"id":"wt-fox","state":"archived","worktree":{"branch_deleted":true,"removed":true}}"#,
        "the answer's bytes"
    );
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["state"], "archived");
    assert_eq!(
        body["worktree"],
        serde_json::json!({ "removed": true, "branch_deleted": true })
    );
    assert!(!path.exists());
    assert_eq!(
        worktree_events(&session),
        vec![WorktreeEventKind::Removed {
            branch_deleted: true
        }],
        "the log accepts the entry after the runtime has shut down"
    );
    host.swansong().shut_down().await;
}

/// A worktree with an untracked file is kept by `PATCH {"state":"archived"}`, and the
/// response says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_dirty_worktree_keeps_it() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "wt-fox").await;
    std::fs::write(path.join("notes.txt"), "work").unwrap();

    let body = server
        .patch("/sessions/wt-fox")
        .with_json_body(&serde_json::json!({"state": "archived"}))
        .await
        .assert_status(200)
        .body()
        .to_string();
    let raw = body.clone();
    let body: Value = serde_json::from_str(&body).unwrap();
    let kept = body["worktree"]["kept"].as_str().unwrap();
    assert!(kept.contains("notes.txt"), "{kept}");
    assert_eq!(
        raw,
        format!(
            r#"{{"id":"wt-fox","resumed":false,"state":"archived","worktree":{{"kept":{}}}}}"#,
            serde_json::to_string(kept).unwrap()
        ),
        "a patch to archived answers `resumed: false` beside the worktree"
    );
    assert!(path.join("notes.txt").is_file());
    host.swansong().shut_down().await;
}

/// A session that did not make a worktree answers without the field.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_without_a_worktree_answers_without_the_field() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt(
        "one".into(),
        runtime::started(runtime::boot(&roots.0.join("one")), host.swansong()).await,
    )
    .expect("the id is free in this test");
    let server = host_server(&host).await;
    let body = server
        .delete("/sessions/one")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert!(json(&body).get("worktree").is_none(), "{body}");
    host.swansong().shut_down().await;
}

/// The listing places a worktree session in its project, so a client scoping to
/// the project's sessions finds it, and names the worktree as `working`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worktree_session_lists_under_its_project() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let path = hosting_worktree(&roots, &host, "wt-lister").await;

    let row = host
        .listing()
        .into_iter()
        .find(|row| row.id == "wt-lister")
        .expect("listed");
    assert_eq!(row.cwd.as_deref(), Some(roots.0.to_str().unwrap()));
    assert_eq!(row.working.as_deref(), Some(path.to_str().unwrap()));
    host.swansong().shut_down().await;
}

/// The incident this occupancy fixes (docs/worktrees.md): a successor that inherits
/// the worktree works in it after the creator's archive — ignored files and all — and
/// the tree goes only when the last occupant leaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_the_creator_keeps_a_worktree_the_successor_works_in() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "quiet-fox").await;
    std::fs::write(roots.0.join(".git/info/exclude"), "tmp/\n").unwrap();
    std::fs::create_dir_all(path.join("tmp")).unwrap();
    std::fs::write(path.join("tmp/checklist.md"), "mine").unwrap();
    hosting_at(&roots, &host, "tidy-moss", &path).await;
    for id in ["quiet-fox", "tidy-moss"] {
        host.session(id)
            .unwrap()
            .append(1, Payload::User(vec![]))
            .unwrap();
    }

    let body = server
        .delete("/sessions/quiet-fox")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert!(
        json(&body).get("worktree").is_none(),
        "the successor still works here: {body}"
    );
    assert!(
        path.join("tmp/checklist.md").is_file(),
        "the ignored checklist survives the creator's archive"
    );

    let body = server
        .delete("/sessions/tidy-moss")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(
        json(&body)["worktree"],
        serde_json::json!({ "removed": true, "branch_deleted": true }),
        "the last occupant's archive retires the tree"
    );
    assert!(!path.exists());
    host.swansong().shut_down().await;
}

/// Several sessions work in one tree: one working outside it never holds it, and the
/// last one at or beneath the root — nested or not — is the one whose archive retires
/// it (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_last_occupant_of_a_tree_retires_it_however_nested() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let path = hosting_worktree(&roots, &host, "quiet-fox").await;
    std::fs::write(roots.0.join(".git/info/exclude"), "src/\n").unwrap();
    let nested = path.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    hosting_at(&roots, &host, "tidy-moss", &nested).await;
    hosting_at(&roots, &host, "warm-fjord", &roots.0).await;
    for id in ["quiet-fox", "tidy-moss", "warm-fjord"] {
        host.session(id)
            .unwrap()
            .append(1, Payload::User(vec![]))
            .unwrap();
    }

    server
        .delete("/sessions/warm-fjord")
        .await
        .assert_status(200);
    assert!(
        path.exists(),
        "a session working outside the tree does not hold it"
    );
    server
        .delete("/sessions/quiet-fox")
        .await
        .assert_status(200);
    assert!(path.exists(), "the nested occupant remains");
    server
        .delete("/sessions/tidy-moss")
        .await
        .assert_status(200);
    assert!(
        !path.exists(),
        "the last occupant's archive retires the tree"
    );
    host.swansong().shut_down().await;
}

/// A former occupant restores a removed tree at its original path, keyed by the
/// tree's own identity — not the resuming session's — and keeps its nested working
/// directory (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inheritor_restores_a_removed_worktree_at_its_root() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let env = crate::config::env_table::ChildEnv::default();
    let added = crate::host::worktree::add(&roots.0, "quiet-fox", &roots.0, &env).unwrap();
    let nested = added.path.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("lib.rs"), "pub fn f() {}").unwrap();
    // The nested working directory is tracked, so the branch the restore re-creates
    // carries it back.
    crate::host::worktree::testing::git(&added.path, &["add", "src"]);
    crate::host::worktree::testing::commit(&added.path, "src");
    dormant(&roots, "tidy-moss", &nested, true).await;

    let retired = crate::host::worktree::retire(&roots.0, "quiet-fox", &env);
    assert_eq!(
        retired,
        crate::host::worktree::Retired::Removed {
            branch_deleted: false
        },
        "the unique commit keeps the branch"
    );
    assert!(!added.path.exists());

    let host = Host::new(boot::test::codex_registry(&roots.0, "host"), baseline_env());
    let server = host_server(&host).await;
    server
        .patch("/sessions/tidy-moss")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": roots.0}))
        .await
        .assert_status(200);
    host.wait_until("tidy-moss hosted", || host.session("tidy-moss").is_some())
        .await;

    assert!(added.path.is_dir(), "the tree is back at its original path");
    assert!(
        nested.join("lib.rs").is_file(),
        "its tracked files are back"
    );
    let session = host.session("tidy-moss").unwrap();
    assert_eq!(session.directories().working(), nested);
    assert!(
        worktree_events(&session)
            .iter()
            .any(|kind| matches!(kind, WorktreeEventKind::Restored(_))),
        "the re-creation is on the resume's log"
    );
    host.swansong().shut_down().await;
}

/// A dormant session that was never archived is an occupant its log still vouches
/// for: another occupant's archive keeps the tree (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dormant_occupant_holds_a_worktree_against_another_archive() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let env = crate::config::env_table::ChildEnv::default();
    let added = crate::host::worktree::add(&roots.0, "quiet-fox", &roots.0, &env).unwrap();
    dormant(&roots, "tidy-moss", &added.path, false).await;

    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    hosting_at(&roots, &host, "warm-fjord", &added.path).await;
    host.session("warm-fjord")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();

    let body = server
        .delete("/sessions/warm-fjord")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert!(
        json(&body).get("worktree").is_none(),
        "the dormant session still works here: {body}"
    );
    assert!(added.path.is_dir());
    host.swansong().shut_down().await;
}

/// A dormant session's archive is its leaving: once its log ends in one, it holds
/// nothing and the last live occupant's archive retires the tree
/// (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archived_dormant_session_holds_nothing() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    let env = crate::config::env_table::ChildEnv::default();
    let added = crate::host::worktree::add(&roots.0, "quiet-fox", &roots.0, &env).unwrap();
    dormant(&roots, "tidy-moss", &added.path, true).await;

    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    hosting_at(&roots, &host, "warm-fjord", &added.path).await;
    host.session("warm-fjord")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();

    let body = server
        .delete("/sessions/warm-fjord")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(
        json(&body)["worktree"],
        serde_json::json!({ "removed": true, "branch_deleted": true }),
        "the archived dormant session holds nothing"
    );
    assert!(!added.path.exists());
    host.swansong().shut_down().await;
}

/// A name an earlier checkout left behind — its branch and its directory — is routed
/// around: the creation gets the first free suffix and touches nothing of the
/// leftover (docs/worktrees.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_left_by_an_earlier_checkout_takes_the_next_suffix() {
    let roots = Roots::new();
    crate::host::worktree::testing::repository(&roots.0);
    crate::host::worktree::testing::git(&roots.0, &["branch", "efference/verbs-4b"]);
    let leftover = roots.0.join(".efference/worktrees/verbs-4b");
    std::fs::create_dir_all(&leftover).unwrap();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;

    let created = server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "verbs-4b",
            "cwd": roots.0,
            "model": "test/m",
            "config": "missing.toml",
            "worktree": true,
        }))
        .await;
    created.assert_status(201);
    assert_eq!(
        json(created.body())["id"],
        "verbs-4b-1",
        "the hold is routed around"
    );
    assert!(leftover.is_dir(), "the leftover is untouched");
    assert_eq!(
        crate::host::worktree::testing::git(&roots.0, &["branch", "--list", "efference/verbs-4b"]),
        "efference/verbs-4b",
        "the leftover branch is untouched"
    );
    host.swansong().shut_down().await;
}
