//! A session that never had a turn is discarded when archived (docs/resumption.md).

use super::*;
use crate::log::Payload;
use crate::session_ids::SessionIds;
use serde_json::Value;

/// A host over a state database in `roots`, and the register it reserves in.
fn host_and_register(roots: &Roots) -> (Arc<Host>, SessionIds) {
    let state = roots.0.join("state.db");
    let host = Host::new(
        one_connection_registry().with_history(crate::history::History::at(state.clone())),
        baseline_env(),
    );
    (host, SessionIds::at(state))
}

/// Host a session named `id` whose directory is `<roots>/.efference/<id>`, over a
/// file log with no entries, with its id reserved.
async fn adopt_stored(host: &Arc<Host>, register: &SessionIds, roots: &Roots, id: &str) -> PathBuf {
    adopt_child(host, register, roots, id, None).await
}

async fn adopt_child(
    host: &Arc<Host>,
    register: &SessionIds,
    roots: &Roots,
    id: &str,
    created_by: Option<&str>,
) -> PathBuf {
    let dir = roots.0.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(crate::session_dir::LOG_NAME);
    let mut boot = runtime::boot(&roots.0.join("one"));
    boot.paths = SessionPaths::new(
        crate::policy::Directories::new(roots.0.clone(), roots.0.join("one")),
        dir,
    )
    .unwrap();
    boot.log = Log::create(&path, None).expect("a fresh log");
    register.reserve(id, &roots.0).unwrap();
    host.adopt_with(
        id.into(),
        runtime::started(boot, host.swansong()).await,
        created_by.map(String::from),
        false,
    )
    .expect("the id is free in this test");
    path
}

/// Host `id` working in a worktree it made, with a log and storage directory.
async fn adopt_worktree(
    host: &Arc<Host>,
    register: &SessionIds,
    roots: &Roots,
    id: &str,
) -> (PathBuf, PathBuf) {
    let env = crate::env_table::ChildEnv::default();
    let added = crate::worktree::add(&roots.0, id, &roots.0, &env).unwrap();
    let dir = roots.0.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut boot = runtime::boot(&roots.0.join("one"));
    boot.paths = SessionPaths::new(
        crate::policy::Directories::new(roots.0.clone(), added.path.clone()),
        dir.clone(),
    )
    .unwrap();
    boot.log = Log::create(dir.join(crate::session_dir::LOG_NAME), None).unwrap();
    register.reserve(id, &roots.0).unwrap();
    host.adopt(id.into(), runtime::started(boot, host.swansong()).await)
        .unwrap();
    (added.path, dir)
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_an_empty_session_removes_it_and_frees_its_id() {
    let roots = Roots::new();
    let (host, register) = host_and_register(&roots);
    let server = host_server(&host).await;
    let path = adopt_stored(&host, &register, &roots, "empty-one").await;
    adopt_stored(&host, &register, &roots, "empty-two").await;
    assert!(register.holder("empty-one").is_some());

    let body = server
        .delete("/sessions/empty-one")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(json(&body)["state"], "discarded");
    assert!(!path.parent().unwrap().exists());
    assert_eq!(
        register.holder("empty-one"),
        None,
        "the id can be minted again"
    );

    // The PATCH spelling discards the same way.
    let body = server
        .patch("/sessions/empty-two")
        .with_json_body(&serde_json::json!({"state": "archived"}))
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(json(&body)["state"], "discarded");
    assert!(!roots.0.join(".efference/empty-two").exists());
    assert_eq!(register.holder("empty-two"), None);
    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_session_with_a_turn_keeps_it() {
    let roots = Roots::new();
    let (host, register) = host_and_register(&roots);
    let server = host_server(&host).await;
    let path = adopt_stored(&host, &register, &roots, "talked").await;
    host.session("talked")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();

    let body = server
        .delete("/sessions/talked")
        .await
        .assert_status(200)
        .body()
        .to_string();
    assert_eq!(json(&body)["state"], "archived");
    let recording = crate::persist::load_recording(&path).unwrap();
    assert!(matches!(
        recording.entries.last().unwrap().payload,
        Payload::Archived { .. }
    ));
    assert!(register.holder("talked").is_some());
    host.swansong().shut_down().await;
}

/// A child ends with its creator by the same path, so an empty child is discarded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_child_is_discarded_with_its_creator() {
    let roots = Roots::new();
    let (host, register) = host_and_register(&roots);
    adopt_stored(&host, &register, &roots, "parent").await;
    host.session("parent")
        .unwrap()
        .append(1, Payload::User(vec![]))
        .unwrap();
    let child = adopt_child(&host, &register, &roots, "child", Some("parent")).await;

    host.end("parent").expect("hosted").await;
    // The discard removes the directory and then releases the id, so both are waited for.
    host.poll_until("the child was discarded", || {
        !child.parent().unwrap().exists() && register.holder("child").is_none()
    })
    .await;
    assert!(!child.parent().unwrap().exists());
    assert_eq!(register.holder("child"), None);
    host.swansong().shut_down().await;
}

/// A worktree with work in it keeps the session: it is archived, not discarded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_session_with_a_dirty_worktree_is_archived() {
    let roots = Roots::new();
    crate::worktree::testing::repository(&roots.0);
    let (host, register) = host_and_register(&roots);
    let server = host_server(&host).await;
    let (worktree, dir) = adopt_worktree(&host, &register, &roots, "wt-fox").await;
    std::fs::write(worktree.join("notes.txt"), "work").unwrap();

    let body = server
        .delete("/sessions/wt-fox")
        .await
        .assert_status(200)
        .body()
        .to_string();
    let body = json(&body);
    assert_eq!(body["state"], "archived");
    assert!(body["worktree"]["kept"].is_string());
    assert!(dir.join(crate::session_dir::LOG_NAME).is_file());
    host.swansong().shut_down().await;
}

/// A clean worktree goes with the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_session_with_a_clean_worktree_is_discarded_with_it() {
    let roots = Roots::new();
    crate::worktree::testing::repository(&roots.0);
    let (host, register) = host_and_register(&roots);
    let server = host_server(&host).await;
    let (worktree, dir) = adopt_worktree(&host, &register, &roots, "wt-fox").await;

    let body = server
        .delete("/sessions/wt-fox")
        .await
        .assert_status(200)
        .body()
        .to_string();
    let body = json(&body);
    assert_eq!(body["state"], "discarded");
    assert_eq!(body["worktree"]["removed"], true);
    assert!(!worktree.exists());
    assert!(!dir.exists());
    host.swansong().shut_down().await;
}
