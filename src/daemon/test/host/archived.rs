//! `GET /sessions?state=archived`: the dormant listing and its filter
//! (docs/control-api.md). The scan itself is `crate::inventory`'s to test; these
//! cover what the route adds — the filter word, the live exclusion, the
//! remembered roots, and the row's `state`.

use super::*;
use crate::log::{ContentBlock, Payload, Title, TitleAuthor};
use crate::persist::Log;

/// A resumable titled log for session `id` under `root`.
fn write_session(root: &Path, id: &str, title: &str) {
    let dir = root.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = Log::create(dir.join("log.jsonl"), None).unwrap();
    log.append(
        0,
        Payload::Preamble {
            system: "system".into(),
            context: Default::default(),
            inference: None,
        },
    )
    .unwrap();
    log.append(0, Payload::User(vec![ContentBlock::Text("hi".into())]))
        .unwrap();
    log.append(
        0,
        Payload::Title(Title {
            text: title.into(),
            author: TitleAuthor::Operator,
        }),
    )
    .unwrap();
}

fn encode(path: &Path) -> String {
    path.to_string_lossy()
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn ids(body: &str) -> Vec<String> {
    json(body)
        .as_array()
        .unwrap_or_else(|| panic!("an array: {body}"))
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archived_rows_are_listed_for_a_root() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let root = roots.0.join("one");
    write_session(&root, "older", "Older work");
    std::thread::sleep(std::time::Duration::from_millis(5));
    write_session(&root, "newer", "Newer work");
    std::fs::create_dir_all(root.join(".efference/broken")).unwrap();
    write_session(&roots.0.join("two"), "elsewhere", "Not listed");

    let url = format!("/sessions?state=archived&cwd={}", encode(&root));
    let response = server.get(&url).await;
    response.assert_status(200);
    assert_eq!(ids(response.body()), ["newer", "older", "broken"]);
    let rows = json(response.body());
    assert_eq!(rows[0]["state"], "archived");
    assert_eq!(rows[0]["title"], "Newer work");
    assert_eq!(rows[0]["cwd"], root.to_string_lossy().as_ref());
    assert!(rows[0]["created_at"].as_u64().unwrap() <= rows[0]["last_active_at"].as_u64().unwrap());
    assert!(rows[0].get("problem").is_none());
    assert_eq!(rows[2]["problem"], "no log.jsonl");
    assert!(rows[2].get("last_active_at").is_none());

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosted_session_is_not_archived() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let live_cwd = roots.0.join("one");
    write_session(&live_cwd, "one", "Running");
    write_session(&live_cwd, "dormant", "Asleep");
    // The same id under a different root is a different session.
    write_session(&roots.0.join("two"), "one", "Same name, other root");
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&live_cwd), host.swansong()),
    )
    .unwrap();
    let server = host_server(&host).await;

    let url = |root: &Path| format!("/sessions?state=archived&cwd={}", encode(root));
    assert_eq!(ids(server.get(&url(&live_cwd)).await.body()), ["dormant"]);
    assert_eq!(
        ids(server.get(&url(&roots.0.join("two"))).await.body()),
        ["one"]
    );

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_cwd_every_remembered_root_is_covered_once() {
    let roots = Roots::new();
    let host = Host::new(
        one_connection_registry()
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        baseline_env(),
    );
    let server = host_server(&host).await;
    write_session(&roots.0.join("one"), "a", "A");
    write_session(&roots.0.join("two"), "b", "B");
    write_session(&roots.0.join("scratch"), "c", "Never launched here");
    let store = crate::remembered::Projects::at(roots.0.join("state.db"));
    for (cwd, config_key, at) in [("one", "k1", 1), ("one", "k2", 2), ("two", "k1", 3)] {
        store.record(&crate::remembered::Project {
            last_used_at: at,
            cwd: roots.0.join(cwd),
            config_key: config_key.into(),
            config: None,
        });
    }

    let response = server.get("/sessions?state=archived").await;
    response.assert_status(200);
    let mut found = ids(response.body());
    found.sort();
    assert_eq!(found, ["a", "b"], "a root remembered twice lists once");

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_state_or_relative_cwd_is_refused() {
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;

    let response = server.get("/sessions?state=dormant").await;
    response.assert_status(400);
    assert!(
        response.body().contains("unknown state"),
        "{}",
        response.body()
    );
    server
        .get("/sessions?state=archived&cwd=relative/path")
        .await
        .assert_status(400);

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_plain_listing_is_unchanged() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let live_cwd = roots.0.join("one");
    write_session(&live_cwd, "dormant", "Asleep");
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&live_cwd), host.swansong()),
    )
    .unwrap();
    let server = host_server(&host).await;

    for url in ["/sessions", "/sessions?state=active"] {
        let response = server.get(url).await;
        response.assert_status(200);
        assert_eq!(ids(response.body()), ["one"], "{url}");
        assert_eq!(json(response.body())[0]["status"], "idle");
        assert!(json(response.body())[0].get("state").is_none());
    }

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_reads_archived_rows() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let root = roots.0.join("one");
    write_session(&root, "asleep", "Asleep");
    let client = host_client(&host, "unused").await;

    let rows = client.archived_sessions(&root).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.id, "asleep");
    assert_eq!(rows[0].row.title.as_deref(), Some("Asleep"));
    assert_eq!(rows[0].state, SessionState::Archived);

    host.swansong().shut_down().await;
}
