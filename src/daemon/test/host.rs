//! One process hosting several sessions behind `/sessions/<id>/…` (docs/control-channel.md).

use super::*;
use crate::daemon::{Host, SessionRuntime};
use serde_json::Value;
use std::path::Path;

struct Roots(PathBuf);

impl Roots {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-host-{nonce:016x}"));
        for label in ["one", "two"] {
            std::fs::create_dir_all(root.join(label).join("scratch")).unwrap();
        }
        Self(root.canonicalize().unwrap())
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The host's socket stack over `host`, as `serve_host` mounts it.
async fn host_server(host: &Arc<Host>, create: bool) -> TestServer<impl trillium::Handler> {
    let host = Arc::clone(host);
    TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(Transport::Socket { auth: None }, App::Host { create }),
    ))
    .await
}

/// The host's socket stack as a streaming client under one session's prefix — the
/// `test_client` pattern, because `TestServer` reads bodies eagerly and never
/// returns on `/events`.
async fn host_client(host: &Arc<Host>, id: &str) -> ControlClient {
    let host = Arc::clone(host);
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(Transport::Socket { auth: None }, App::Host { create: true }),
    );
    let context = trillium_testing::init(&mut handler).await;
    ControlClient::with_connector(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
    .with_prefix(&format!("/sessions/{id}"))
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

/// Two sessions behind one listener: the list names both, each `/sessions/<id>/v1/state`
/// is that session's (its own cwd), an unknown id is `404`, and ending one leaves the
/// other listed and answering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_routes_each_prefix_to_its_own_session() {
    let roots = Roots::new();
    let host = Host::new();
    for label in ["one", "two"] {
        let runtime = SessionRuntime::start(runtime::boot(&roots.0.join(label)), host.swansong());
        host.adopt(label.into(), runtime);
    }
    let server = host_server(&host, true).await;

    let list = server.get("/sessions").await;
    list.assert_status(200);
    assert_eq!(
        json(list.body()),
        serde_json::json!([{"id": "one"}, {"id": "two"}])
    );

    for label in ["one", "two"] {
        let state = server.get(&format!("/sessions/{label}/v1/state")).await;
        state.assert_status(200);
        let cwd = json(state.body())["session_identity"]["cwd"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            cwd.ends_with(label),
            "{label}'s state, not a sibling's: {cwd}"
        );
    }
    server
        .get("/sessions/nope/v1/state")
        .await
        .assert_status(404)
        .assert_body_contains(r#""code":"not_found""#);
    // A prefix path the session's router does not serve is the session's 404, not the host's.
    server
        .get("/sessions/one/v1/nothing")
        .await
        .assert_status(404);

    let ended = server.delete("/sessions/one").await;
    ended.assert_status(200);
    assert!(
        !roots.0.join("one/scratch").exists(),
        "teardown ran before the answer"
    );
    assert!(roots.0.join("two/scratch").exists());
    tokio::time::timeout(Duration::from_secs(5), async {
        while host.ids() != ["two"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the ended session leaves the list once its loop has returned");
    server
        .get("/sessions/one/v1/state")
        .await
        .assert_status(404);
    server
        .get("/sessions/two/v1/state")
        .await
        .assert_status(200);
    server.delete("/sessions/one").await.assert_status(404);

    host.swansong().shut_down().await;
}

/// The event stream a TUI lives on works through the prefix dispatch: a client based
/// at `/sessions/<id>` opens `/v1/events` and receives that session's frames, replay
/// and live tail alike, with the frame envelope intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prefixed_client_streams_its_sessions_events() {
    let roots = Roots::new();
    let host = Host::new();
    let runtime = SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong());
    let session = Arc::clone(runtime.session());
    host.adopt("one".into(), runtime);
    session.append(1, user("first")).unwrap();

    let client = host_client(&host, "one").await;
    let mut events = client
        .events(Some(0), Some("log"))
        .await
        .expect("open events under the prefix");
    let (id, kind, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("0"));
    assert_eq!(kind.as_deref(), Some("user"));
    let frame: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(frame["class"], "log");
    assert!(frame["data"].to_string().contains("first"));
    session.append(2, user("second")).unwrap();
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("1"));
    assert!(data.contains("second"));

    let state = client.state().await.unwrap();
    assert!(state.contains("\"cwd\""), "{state}");

    host.swansong().shut_down().await;
}

/// Creation is validated before anything is prepared: a malformed body, an id that
/// is not a plain segment, a relative cwd, a taken id, and a config that does not
/// exist each answer `400` with the reason and mint nothing. The network listener
/// refuses creation outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_validates_its_body_and_is_socket_only() {
    let roots = Roots::new();
    let host = Host::new();
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
    );
    let server = host_server(&host, true).await;
    let base = |name: &str, cwd: &Path| {
        serde_json::json!({
            "name": name, "cwd": cwd, "model": "m",
            "env": {"EFFERENCE_PROVIDER": "anthropic", "EFFERENCE_API_KEY": "k"},
        })
    };
    let cases: Vec<(Value, &str)> = vec![
        (serde_json::json!({"cwd": "/x"}), "invalid body"),
        (base("../up", &roots.0), "not a single plain path segment"),
        (base("fresh", Path::new("relative")), "cwd must be absolute"),
        (base("one", &roots.0), "already hosted"),
        (
            {
                let mut b = base("fresh", &roots.0.join("two"));
                b["config"] = Value::String("missing.toml".into());
                b
            },
            "could not prepare session",
        ),
    ];
    for (body, reason) in cases {
        server
            .post("/sessions")
            .with_json_body(&body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert!(
        !roots.0.join("two/.efference").exists(),
        "a failed creation mints nothing"
    );
    assert_eq!(host.ids(), ["one"]);

    let network = host_server(&host, false).await;
    network
        .post("/sessions")
        .with_json_body(&base("fresh", &roots.0.join("two")))
        .await
        .assert_status(403)
        .assert_body_contains("Unix socket only");

    host.swansong().shut_down().await;
}
