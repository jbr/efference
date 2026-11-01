//! Archiving a session that made its own git worktree (docs/control-api.md): the
//! response's `worktree` field, the directory's fate, and the log entry. Hosts here
//! boot sessions that never reach a provider; the worktree is made with
//! `crate::host::worktree::add` and the session's directories pointed at it.

use super::*;
use crate::record::log::{Payload, WorktreeEventKind};
use serde_json::Value;

/// Host a session named `id` whose working directory is a worktree it made.
async fn hosting_worktree(roots: &Roots, host: &Arc<Host>, id: &str) -> PathBuf {
    let env = crate::config::env_table::ChildEnv::default();
    let added = crate::host::worktree::add(&roots.0, id, &roots.0, &env).unwrap();
    let mut boot = runtime::boot(&roots.0.join("one"));
    boot.paths = SessionPaths::new(
        crate::config::policy::Directories::new(roots.0.clone(), added.path.clone()),
        roots.0.join(".efference").join(id),
    )
    .unwrap();
    host.adopt(id.into(), runtime::started(boot, host.swansong()).await)
        .expect("the id is free in this test");
    added.path
}

fn worktree_events(session: &Session) -> Vec<WorktreeEventKind> {
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
