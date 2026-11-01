//! `GET /sessions?state=archived` and `GET /sessions/:id`: the dormant listing, its
//! filter, and the single read (docs/control-api.md). The scan itself is
//! `crate::inventory`'s to test; these cover what the routes add — the filter word,
//! the live exclusion, the remembered roots, the row's `state`, and the single read's
//! recorded selections and context size.

use super::*;
use crate::protocol::sessions::{DormantSession, SessionState};
use crate::record::log::{Payload, Title, TitleAuthor};
use crate::record::persist::Log;

/// A resumable titled log for session `id` under `root`.
pub(super) fn write_session(root: &Path, id: &str, title: &str) {
    let dir = root.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = Log::create(dir.join("log.jsonl"), None).unwrap();
    log.append(0, Payload::preamble("", "system", Default::default(), None))
        .unwrap();
    log.append(0, Payload::user("hi")).unwrap();
    log.append(0, Payload::Title(Title::new(title, TitleAuthor::Operator)))
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
    std::thread::sleep(Duration::from_millis(5));
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
        runtime::started(runtime::boot(&live_cwd), host.swansong()).await,
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
        runtime::started(runtime::boot(&live_cwd), host.swansong()).await,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_session_reads_as_its_hosted_or_dormant_row() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let root = roots.0.join("one");
    write_session(&root, "dormant", "Asleep");
    host.adopt(
        "live".into(),
        runtime::started(runtime::boot(&root), host.swansong()).await,
    )
    .unwrap();
    let server = host_server(&host).await;

    let live = server.get("/sessions/live").await;
    live.assert_status(200);
    let row = json(live.body());
    assert_eq!(row["id"], "live");
    assert_eq!(row["status"], "idle");
    assert_eq!(row["cwd"], root.to_string_lossy().as_ref());

    let url = format!("/sessions/dormant?cwd={}", encode(&root));
    let dormant = server.get(&url).await;
    dormant.assert_status(200);
    let row = json(dormant.body());
    assert_eq!(row["id"], "dormant");
    assert_eq!(row["state"], "archived");
    assert_eq!(row["title"], "Asleep");
    assert!(row.get("status").is_none());

    // The dormant row carries what the listing's does, `problem` included.
    std::fs::create_dir_all(root.join(".efference/broken")).unwrap();
    let url = format!("/sessions/broken?cwd={}", encode(&root));
    let broken = server.get(&url).await;
    broken.assert_status(200);
    assert_eq!(json(broken.body())["problem"], "no log.jsonl");

    let url = format!("/sessions/nobody?cwd={}", encode(&root));
    server.get(&url).await.assert_status(404);
    server.get("/sessions/nobody").await.assert_status(404);
    server
        .get("/sessions/dormant?cwd=relative/path")
        .await
        .assert_status(400);

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dormant_session_is_found_under_a_remembered_root() {
    let roots = Roots::new();
    let host = Host::new(
        one_connection_registry()
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        baseline_env(),
    );
    let server = host_server(&host).await;
    write_session(&roots.0.join("two"), "b", "B");
    crate::remembered::Projects::at(roots.0.join("state.db")).record(&crate::remembered::Project {
        last_used_at: 1,
        cwd: roots.0.join("two"),
        config_key: "k".into(),
        config: None,
    });

    let response = server.get("/sessions/b").await;
    response.assert_status(200);
    assert_eq!(json(response.body())["title"], "B");

    host.swansong().shut_down().await;
}

/// A resumable log for session `id` under `root` whose header records its roles,
/// main `c/main` and spotter `c/spot` with separate triage on `c/tri`, followed by a
/// preamble that switched the spotter to `c/spot-2` at a resume.
fn write_recorded_session(root: &Path, id: &str) -> PathBuf {
    use crate::record::log::{LoggedInference, RecordedRoles, TriageModel};
    use crate::record::persist::SessionMetadata;
    let inference =
        |model: &str| LoggedInference::new("c", crate::record::log::Provider::Anthropic, model);
    let dir = root.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let metadata = SessionMetadata::new(
        id,
        root,
        inference("main"),
        inference("spot"),
        TriageModel::separate(inference("tri")),
    );
    let mut log = Log::create(dir.join("log.jsonl"), Some(metadata)).unwrap();
    let preamble = Payload::preamble("", "system", Default::default(), Some(inference("main")));
    log.append(0, preamble.clone()).unwrap();
    log.append(0, Payload::user("hi")).unwrap();
    let switched = preamble.with_roles(RecordedRoles {
        spotter: inference("spot-2"),
        triage: TriageModel::separate(inference("tri")),
    });
    log.append(0, switched).unwrap();
    dir
}

/// Append usage lines for `requests`, each an origin and its uncached input tokens,
/// with 1000 tokens read from the cache and 10 written.
fn write_usage(dir: &Path, requests: &[(RequestOrigin, u64)]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(USAGE_NAME))
        .unwrap();
    for (at, (origin, input)) in requests.iter().enumerate() {
        let usage = RequestUsage::new(Usage {
            input_tokens: *input,
            output_tokens: 7,
            cache_read_input_tokens: 1000,
            cache_creation_input_tokens: 10,
        });
        let line = UsageLine::new(*origin, at as u128, false, &usage);
        writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dormant_row_carries_its_recorded_selections_and_context_size() {
    use crate::record::log::RequestOrigin;
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let root = roots.0.join("one");
    let dir = write_recorded_session(&root, "recorded");
    write_usage(
        &dir,
        &[
            (RequestOrigin::Turn, 1),
            (RequestOrigin::Turn, 2),
            (RequestOrigin::Triage, 3),
            (RequestOrigin::Keepalive, 4),
        ],
    );

    let response = server
        .get(&format!("/sessions/recorded?cwd={}", encode(&root)))
        .await;
    response.assert_status(200);
    let row = json(response.body());
    // The main from the header, the spotter from the preamble a resume switched it in,
    // the triage from the header: the selections a resume without a switch reopens on.
    assert_eq!(
        row["selections"],
        serde_json::json!({
            "connection": "c",
            "model": "main",
            "spotter_connection": "c",
            "spotter_model": "spot-2",
            "triage": {
                "mode": "separate",
                "connection": "c",
                "model": "tri",
                "thinking": false,
                "auto_choice": false,
                "effort": null,
            },
        })
    );
    // The last `turn` line's prompt: 2 uncached + 1000 read + 10 written.
    assert_eq!(row["context_tokens"], 1012);
    let read: DormantSession = serde_json::from_str(response.body()).unwrap();
    assert_eq!(read.archived.row.id, "recorded");
    assert_eq!(read.context_tokens, Some(1012));

    // The listing's row is the single read's without the two fields.
    let listing = server
        .get(&format!("/sessions?state=archived&cwd={}", encode(&root)))
        .await;
    listing.assert_status(200);
    let mut expected = row.as_object().unwrap().clone();
    expected.remove("selections");
    expected.remove("context_tokens");
    assert_eq!(json(listing.body())[0], Value::Object(expected));

    host.swansong().shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dormant_row_without_a_turn_or_recorded_roles_leaves_them_out() {
    use crate::record::log::RequestOrigin;
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let root = roots.0.join("one");
    // No usage record at all.
    let dir = write_recorded_session(&root, "unused");
    // A usage record with no main-loop request in it, under a header that records nothing.
    write_session(&root, "bare", "Bare");
    write_usage(&root.join(".efference/bare"), &[(RequestOrigin::Title, 5)]);

    let unused = server
        .get(&format!("/sessions/unused?cwd={}", encode(&root)))
        .await;
    unused.assert_status(200);
    let row = json(unused.body());
    assert!(row.get("context_tokens").is_none(), "{row}");
    assert_eq!(row["selections"]["model"], "main");
    assert!(!dir.join(USAGE_NAME).exists());

    let bare = server
        .get(&format!("/sessions/bare?cwd={}", encode(&root)))
        .await;
    bare.assert_status(200);
    let row = json(bare.body());
    assert_eq!(row["title"], "Bare");
    assert!(row.get("context_tokens").is_none(), "{row}");
    assert!(row.get("selections").is_none(), "{row}");

    host.swansong().shut_down().await;
}

/// Host `id` over a file log under `root`'s `.efference/<id>/`, with one user turn so
/// the archive records rather than discards the session.
async fn adopt_in_root(host: &Arc<Host>, root: &Path, id: &str) {
    let dir = root.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut boot = runtime::boot(root);
    boot.log = Log::create(dir.join("log.jsonl"), None).expect("a fresh log");
    host.adopt(id.into(), runtime::started(boot, host.swansong()).await)
        .expect("the id is free in this test");
    host.session(id)
        .unwrap()
        .append(1, Payload::user("hi"))
        .unwrap();
}

/// A client's `source` on `DELETE /sessions/:id` is recorded on the `Archived` entry,
/// the dormant row reads it back as `archived_by`, and the TUI's question after the
/// stream ends is answered with the archiver and the log's directory
/// (docs/log-and-projection.md, docs/tui.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_records_who_asked_and_the_tui_can_read_it_back() {
    use crate::tui::closed::{Closure, ask};

    let roots = Roots::new();
    let root = roots.0.join("one");
    let host = Host::new(
        one_connection_registry()
            .with_history(crate::history::History::at(roots.0.join("state.db"))),
        baseline_env(),
    );
    crate::remembered::Projects::at(roots.0.join("state.db")).record(&crate::remembered::Project {
        last_used_at: 1,
        cwd: root.clone(),
        config_key: "k".into(),
        config: None,
    });
    adopt_in_root(&host, &root, "hold-probe").await;
    adopt_in_root(&host, &root, "anonymous").await;
    adopt_in_root(&host, &root, "patched").await;
    let client = host_client(&host, "hold-probe").await;

    client
        .archive_session("hold-probe", Some("mcp[claude-code]"))
        .await
        .unwrap();
    client.archive_session("anonymous", None).await.unwrap();
    let server = host_server(&host).await;
    server
        .patch("/sessions/patched")
        .with_json_body(&serde_json::json!({"state": "archived", "source": "web"}))
        .await
        .assert_status(200);

    let last = |id: &str| {
        let path = root.join(".efference").join(id).join("log.jsonl");
        crate::record::persist::load_recording(path)
            .unwrap()
            .entries
            .last()
            .unwrap()
            .payload
            .clone()
    };
    assert!(matches!(
        last("hold-probe"),
        Payload::Archived { source: Some(ref source) } if source == "mcp[claude-code]"
    ));
    assert!(matches!(
        last("anonymous"),
        Payload::Archived { source: None }
    ));
    assert!(matches!(
        last("patched"),
        Payload::Archived { source: Some(ref source) } if source == "web"
    ));

    let closure = ask(&client).await;
    assert_eq!(
        closure,
        Closure::Archived {
            id: "hold-probe".into(),
            by: Some("mcp[claude-code]".into()),
            log: root.join(".efference/hold-probe"),
        }
    );
    assert_eq!(
        closure.line(),
        format!(
            "efference: hold-probe was archived by mcp[claude-code]; its log stays at {}/",
            root.join(".efference/hold-probe").display()
        )
    );
    let anonymous = ask(&host_client(&host, "anonymous").await).await;
    assert!(matches!(anonymous, Closure::Archived { by: None, .. }));

    host.swansong().shut_down().await;
}
