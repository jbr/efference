//! One process hosting several sessions behind `/sessions/<id>/…` (docs/control-channel.md).

use super::*;
use crate::daemon::{Host, SessionRuntime};
use serde_json::Value;
use std::path::Path;
use trillium::KnownHeaderName;

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
/// A host registry with one env-backed connection, so creation gets past role
/// resolution and into `boot::prepare`. The credential never reaches a provider:
/// every case here fails before the first request.
fn one_connection_registry() -> crate::inference::InferenceRegistry {
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[connections.test]
provider = "anthropic"
auth = { kind = "env", key = "TEST_KEY" }
"#,
    )
    .unwrap();
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    crate::inference::InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
}

/// A registry with one env-backed connection and an `env_hook`, so a creation
/// that omits its environment runs it (docs/env.md).
fn registry_with_hook(argv: &[String]) -> crate::inference::InferenceRegistry {
    let listed = argv
        .iter()
        .map(|part| format!("{part:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let config = crate::inference::InferenceConfig::from_toml(&format!(
        "\nenv_hook = [{listed}]\n\n\
         [connections.test]\n\
         provider = \"anthropic\"\n\
         auth = {{ kind = \"env\", key = \"TEST_KEY\" }}\n"
    ))
    .expect("a config naming a hook parses");
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    crate::inference::InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap()
}

/// A host environment carrying one marker, so a hook's inheritance of the launch
/// baseline is observable (docs/env.md).
fn marked_baseline() -> crate::env_table::EnvSnapshot {
    crate::env_table::EnvSnapshot::new(
        [("FROM_BASELINE".to_string(), "inherited".to_string())].into(),
    )
    .expect("a valid environment")
}

/// A creation body that stops at `boot::prepare` rather than reaching a
/// provider: the config does not exist, so the request fails for a reason every
/// environment branch shares.
fn stops_at_boot(cwd: &Path) -> Value {
    serde_json::json!({
        "name": "fresh",
        "cwd": cwd,
        "model": "test/m",
        "config": "missing.toml",
    })
}

async fn host_server(host: &Arc<Host>) -> TestServer<impl trillium::Handler> {
    let host = Arc::clone(host);
    TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(Transport::Socket { auth: None }, App::Host),
    ))
    .await
}

/// The host routes as a network listener mounts them: the web transport, behind
/// its authenticator, so what differs from `host_server` is the transport rather
/// than which routes exist (docs/control-channel.md, docs/web-bind.md). No socket
/// is bound — the transport is in memory, as in the TCP tests.
async fn network_host_server(host: &Arc<Host>) -> (TestServer<impl trillium::Handler>, String) {
    use crate::auth::{BurnLedger, Root};
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let root = std::env::temp_dir().join(format!("efference-net-host-{nonce:016x}"));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (auth, _listener) = crate::daemon::tcp::arm_with_ledger(
        crate::daemon::tcp::TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            public_url: "http://localhost:7777".into(),
            root: Root::new(root.to_string_lossy().as_ref()),
            open: false,
        },
        BurnLedger::new(root.join("burns")),
    );
    let host = Arc::clone(host);
    let server = TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(
            Transport::Web {
                auth: Arc::clone(&auth),
            },
            App::Host,
        ),
    ))
    .await
    // The gate answers only for a host this daemon's name covers; the in-memory
    // server presents one it does not otherwise (as in the TCP tests).
    .with_host("localhost");
    let bearer = format!("Bearer {}", auth.bearer());
    (server, bearer)
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
        handler(Transport::Socket { auth: None }, App::Host),
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

/// An omitted environment is established for the request's own directory: the
/// hook runs there, inheriting the launch baseline, and its failure fails the
/// creation before anything is minted (docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_omitted_environment_runs_the_hook_for_that_directory() {
    let roots = Roots::new();
    let ran = roots.0.join("hook-ran");
    // Records what it was given, then fails: a hook's stderr is discarded, so the
    // exit code is all the host has to report.
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "printf 'cwd=%s\\nhost=%s\\n' \"$PWD\" \"$FROM_BASELINE\" > {}; exit 7",
            ran.display()
        ),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&roots.0.join("one")))
        .await
        .assert_status(400)
        .assert_body_contains("exited 7");
    assert!(host.ids().is_empty(), "a failed launch mints no session");
    assert!(
        !roots.0.join("one/.efference").exists(),
        "a failed launch leaves no session directory"
    );

    let seen = std::fs::read_to_string(&ran).expect("the hook ran");
    assert!(
        seen.contains(&format!("cwd={}", roots.0.join("one").display())),
        "the hook ran in the session's own directory: {seen}"
    );
    assert!(
        seen.contains("host=inherited"),
        "the hook inherited the host's launch baseline: {seen}"
    );
}

/// A supplied environment is the base outright: the hook is never run, so a
/// request that brings its own environment is unaffected by a broken one
/// (docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supplied_environment_never_runs_the_hook() {
    let roots = Roots::new();
    let ran = roots.0.join("hook-ran");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(": > {}; exit 7", ran.display()),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "fresh",
            "cwd": roots.0.join("one"),
            "model": "test/m",
            "config": "missing.toml",
            "env": {"HOME": "/h"},
        }))
        .await
        // Fails at boot, on the same missing config every branch here uses.
        .assert_status(400)
        .assert_body_contains("could not prepare session");
    assert!(
        !ran.exists(),
        "a request carrying its own environment must not run the hook"
    );
    assert!(
        host.ids().is_empty(),
        "a failed preparation mints no session"
    );
}

/// `QUERY /sessions` stays a preview: it resolves options and nothing else, so it
/// establishes no environment and runs no hook. Minting nothing is not by itself
/// evidence of that — the hook writes a marker, and no marker means it never ran
/// (docs/control-api.md, docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preview_never_runs_the_hook() {
    let roots = Roots::new();
    let ran = roots.0.join("hook-ran");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(": > {}", ran.display()),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    // No `env` at all, which is exactly the request a preview would establish an
    // environment for if it did.
    server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!({
            "cwd": roots.0.join("one"),
            "connection": "test",
            "model": "m",
        }))
        .await
        .assert_status(200);

    assert!(host.ids().is_empty(), "a preview mints nothing");
    assert!(
        !ran.exists(),
        "a preview must not establish an environment, so it must not run the hook"
    );
    host.swansong().shut_down().await;
}

/// Host shutdown cancels an establishment in progress, and the creation fails
/// without minting. The hook here is killed mid-run — nothing about that rolls
/// back whatever the hook had already done outside the process (docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_shutdown_cancels_an_establishment_in_progress() {
    let roots = Roots::new();
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ];
    let host = Host::new(registry_with_hook(&hook), marked_baseline());
    let server = host_server(&host).await;

    // Shut down while the hook is still running: the hook outlives a test's
    // patience, so the cancellation is the only way this request ends.
    let shutting_down = Arc::clone(&host);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        shutting_down.swansong().shut_down().await;
    });

    server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&roots.0.join("one")))
        .await
        .assert_status(400)
        .assert_body_contains("cancelled");
    assert!(
        host.ids().is_empty(),
        "a cancelled establishment mints no session"
    );
    assert!(
        !roots.0.join("one/.efference").exists(),
        "and leaves no session directory"
    );
}

/// With no hook there is nothing to run: the launch baseline is the host's own
/// environment, read directly, and creation proceeds past environment
/// establishment to the same failure every other branch reaches (docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_hook_the_host_environment_is_the_base() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), marked_baseline());
    let server = host_server(&host).await;

    let response = server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&roots.0.join("one")))
        .await;
    response.assert_status(400);
    let body = response.body();
    assert!(
        body.contains("could not prepare session"),
        "an omitted environment with no hook reaches boot rather than failing at \
         environment establishment: {body}"
    );
    assert!(
        !body.contains("env:"),
        "and reports nothing from environment establishment: {body}"
    );
    assert!(
        host.ids().is_empty(),
        "a failed preparation mints no session"
    );
}

/// `QUERY /sessions` (docs/control-api.md): the host's preview of a proposed
/// session — the effective selections, where each came from, and the issues that
/// would stop creation — without minting anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_previews_a_proposed_session_without_creating_one() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let client = host_client(&host, "one").await.with_prefix("");
    let mut request = SessionOptionsRequest::new(roots.0.join("one"));
    request.options.connection = Some("test".to_string());
    request.options.model = Some("m".to_string());
    let preview = client
        .session_options(&request)
        .await
        .expect("preview a proposed session");
    // Asserted through a re-encode: `preview` is the typed `ResolvedOptions` the
    // client decoded, so matching the wire again proves both the host's encoding
    // and that the client's decode of it loses nothing.
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        serde_json::json!({
            "effective": {
                "main": {
                    "value": {"connection": "test", "provider": "anthropic", "model": "m"},
                    "source": "request",
                },
                "spotter": {
                    "value": {
                        "connection": "test",
                        "provider": "anthropic",
                        "model": "claude-haiku-4-5",
                    },
                    "source": "main_provider",
                },
                "triage": {"value": {"mode": "shared"}, "source": "shared_main"},
                "effort": {"value": null, "source": "provider_default"},
                "temperature": {"value": null, "source": "provider_default"},
            },
            "issues": [],
        })
    );
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}

/// **One bad field does not hide the good ones.** A malformed temperature is its
/// own issue; every independently resolvable selection still comes back, because
/// the form has to stay usable while the operator is mid-edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_reports_an_invalid_field_without_hiding_the_rest() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let response = server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!({
            "cwd": roots.0.join("one"),
            "connection": "test",
            "model": "m",
            "temperature": "260",
            "effort": "high",
        }))
        .await;
    response.assert_status(200);
    let body = json(response.body());
    assert_eq!(
        body["issues"],
        serde_json::json!([{
            "field": "temperature",
            "code": "invalid_type",
            "message": "must be a whole number from 0 to 100",
        }])
    );
    assert_eq!(body["effective"]["main"]["value"]["model"], "m");
    assert_eq!(
        body["effective"]["effort"],
        serde_json::json!({"value": "high", "source": "request"})
    );
    // The field that failed is the only one with no source.
    assert_eq!(
        body["effective"]["temperature"],
        serde_json::json!({"value": null, "source": null})
    );
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}

/// A body that is not a JSON object has no fields to resolve independently, so it
/// is the one shape that answers `400` rather than a preview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_refuses_a_non_object_body() {
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!(["not", "an", "object"]))
        .await
        .assert_status(400);
    host.swansong().shut_down().await;
}

/// `QUERY /sessions` mints nothing and establishes no environment, so it stays a
/// pure preview on a network listener (docs/control-api.md). `POST /sessions`
/// there behaves as it does on the socket: the route is one, and what a request
/// omits about its environment is the host's business rather than the
/// listener's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_network_host_previews_and_creates_the_same_way() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let (server, bearer) = network_host_server(&host).await;
    let response = server
        .build(trillium::Method::Query, "/sessions")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_json_body(&serde_json::json!({"cwd": "/project", "connection": "test", "model": "m"}))
        .await;
    response.assert_status(200);
    assert_eq!(
        json(response.body())["effective"]["main"]["value"]["model"],
        "m"
    );
    assert!(
        host.ids().is_empty(),
        "a preview establishes no environment and mints nothing"
    );

    // Creation is mounted here, and validates the same way: a relative cwd is the
    // same `400` the socket answers, not a refusal for arriving over TCP. A
    // session is not minted — these tests stop before `boot::prepare`, which
    // would reach a provider.
    let created = server
        .post("/sessions")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .with_json_body(&serde_json::json!({
            "name": "fresh",
            "cwd": "relative",
            "model": "test/m",
            "env": {"HOME": "/h"},
        }))
        .await;
    assert_eq!(
        created.status(),
        400,
        "network creation: {}",
        created.body()
    );
    assert!(
        created.body().contains("cwd must be absolute"),
        "the same reason the socket gives: {}",
        created.body()
    );
    assert!(host.ids().is_empty(), "a rejected creation mints nothing");
    let _ = roots;
    host.swansong().shut_down().await;
}

/// `GET /inference` is a host inventory, never a projection of its credential
/// sources or their values. Rows are sorted by name, and a disabled connection is
/// listed as unavailable rather than omitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inference_inventory_is_sorted_and_secret_free() {
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[connections.test]
provider = "anthropic"
auth = { kind = "env", key = "TEST_KEY" }

[connections.also]
provider = "openai"
auth = { kind = "env", key = "TEST_KEY" }
disabled = true
"#,
    )
    .unwrap();
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    let host = Host::new(
        crate::inference::InferenceRegistry::from_config(
            config,
            Path::new("/host"),
            env,
            &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
        )
        .unwrap(),
        baseline_env(),
    );
    let client = host_client(&host, "one").await.with_prefix("");
    let inventory = client
        .inference_inventory()
        .await
        .expect("read inference inventory");
    assert_eq!(
        serde_json::to_value(&inventory).unwrap(),
        serde_json::json!([
            {"name": "also", "provider": "openai", "available": false},
            {"name": "test", "provider": "anthropic", "available": true},
        ])
    );
    // The row type carries three plain fields, so a credential could only reach a
    // client by being added to it — which the structural assertion above would
    // fail to compile against. This scans the encoding anyway, because the
    // property is about bytes leaving the host.
    assert!(
        !serde_json::to_string(&inventory)
            .unwrap()
            .contains("test-secret")
    );
    host.swansong().shut_down().await;
}

/// The listing is one resource with two readings: `GET /sessions` answers the
/// array, and the same path with `Accept: text/event-stream` subscribes to it —
/// the current value on connect, then the whole value again on every change,
/// with no cursor and nothing to reconcile (`host/summary.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_listing_streams_its_current_value() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
    );
    // A root-based client: the listing is the *listener's* surface, not a
    // session's, so it sits at the socket root under either binding.
    let client = host_client(&host, "one").await.with_prefix("");
    let mut events = client
        .session_events()
        .await
        .expect("subscribe to the listing");

    let (id, kind, data) = next_event(&mut events).await;
    assert_eq!(kind.as_deref(), Some("sessions"));
    // No cursor on a resource with no history: an `id` here would be a resume
    // position into a stream that cannot replay.
    assert_eq!(id, None);
    let listing = json(&data);
    assert_eq!(listing[0]["id"], "one");
    assert_eq!(listing[0]["status"], "idle");
    assert_eq!(listing[0]["processes"], 0);
    assert_eq!(listing.as_array().unwrap().len(), 1);

    // A second session is a change to the listing, so the next frame carries
    // both rows rather than a delta naming the arrival.
    host.adopt(
        "two".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("two")), host.swansong()),
    );
    let (_, _, data) = next_event(&mut events).await;
    let ids: Vec<String> = json(&data)
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["one", "two"]);

    host.swansong().shut_down().await;
}

/// Two sessions behind one listener: the list names both, each `/sessions/<id>/v1/state`
/// is that session's (its own cwd), an unknown id is `404`, and ending one leaves the
/// other listed and answering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_routes_each_prefix_to_its_own_session() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    for label in ["one", "two"] {
        let runtime = SessionRuntime::start(runtime::boot(&roots.0.join(label)), host.swansong());
        let session = Arc::clone(runtime.session());
        host.adopt(label.into(), runtime);
        // The host names each adopted session's web path — what a push
        // notification opens (docs/web-push.md).
        assert_eq!(session.web_path(), format!("/sessions/{label}/"));
    }
    let server = host_server(&host).await;

    // Each entry carries the session's cwd — what a client scopes a shared
    // host's listing to its own project by (docs/control-channel.md).
    let list = server.get("/sessions").await;
    list.assert_status(200);
    let cwd = |label: &str| roots.0.join(label).to_string_lossy().into_owned();
    assert_eq!(
        json(list.body()),
        serde_json::json!([
            {"id": "one", "cwd": cwd("one"), "status": "idle", "processes": 0},
            {"id": "two", "cwd": cwd("two"), "status": "idle", "processes": 0},
        ])
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
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
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
/// exist each answer `400` with the reason and mint nothing. Validation is a
/// property of the request, so a network listener answers the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_validates_its_body_on_every_listener() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
    );
    let server = host_server(&host).await;
    let base = |name: &str, cwd: &Path| {
        serde_json::json!({
            "name": name, "cwd": cwd, "model": "test/m",
            "env": {"HOME": "/h"},
        })
    };
    let cases: Vec<(Value, &str)> = vec![
        // Not an object at all. A body that merely omits `env` is well-formed —
        // that omission is what asks the host to establish one (docs/env.md).
        (serde_json::json!(["not", "an", "object"]), "invalid body"),
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
    for (body, reason) in &cases {
        server
            .post("/sessions")
            .with_json_body(body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert!(
        !roots.0.join("two/.efference").exists(),
        "a failed creation mints nothing"
    );
    assert_eq!(host.ids(), ["one"]);

    // The same rejections on a network listener: same status, same reasons.
    let (network, bearer) = network_host_server(&host).await;
    for (body, reason) in &cases {
        network
            .post("/sessions")
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .with_json_body(body)
            .await
            .assert_status(400)
            .assert_body_contains(reason);
    }
    assert_eq!(host.ids(), ["one"]);

    host.swansong().shut_down().await;
}

/// Two hosted sessions select different provider/auth/model tuples from the one
/// host registry, `session_identity` names every role by connection, a triage
/// selection on a foreign dialect is refused before anything is minted, and the
/// host's credentials reach neither the log, the state, nor a tool child's
/// environment (docs/transport.md, docs/env.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_sessions_select_roles_from_the_host_registry() {
    let roots = Roots::new();
    let home = roots.0.join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(
        home.join(".codex/auth.json"),
        serde_json::json!({
            "tokens": {"access_token": "codex-canary-token", "account_id": "acct-1"}
        })
        .to_string(),
    )
    .unwrap();
    let config_path = home.join(".efference/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        r#"
[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }

[connections.platform]
provider = "openai"
auth = { kind = "env", key = "PLATFORM_KEY" }

[connections.messages]
provider = "anthropic"
auth = { kind = "env", key = "MESSAGES_KEY" }
"#,
    )
    .unwrap();
    let host_env = crate::env_table::EnvSnapshot::new(
        [
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            (
                "PLATFORM_KEY".to_string(),
                "platform-canary-secret".to_string(),
            ),
            (
                "MESSAGES_KEY".to_string(),
                "messages-canary-secret".to_string(),
            ),
        ]
        .into(),
    )
    .unwrap();
    // The Codex connection's capabilities come from the provider's catalog, so
    // the test seeds the cache that lookup reads: these sessions boot without a
    // provider. Seeded under the account `auth.json` above names.
    crate::api::seed_codex_catalog(crate::api::Provider::OpenAICodex, "acct-1");
    let registry = crate::inference::InferenceRegistry::load(
        &roots.0,
        host_env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .expect("host registry");
    let host = Host::new(registry, baseline_env());
    let server = host_server(&host).await;

    // Each project's `[context]` runs `env` in a tool child, so a host secret that
    // leaked into the child environment would land — masked — in the boot preamble.
    for label in ["one", "two"] {
        std::fs::write(
            roots.0.join(label).join("efference.toml"),
            "scope = 'test'\ntools = ['read']\n[paths]\n'.' = true\n[context]\nChildEnv = { exec = \"env\" }\n",
        )
        .unwrap();
    }
    let caller_env = serde_json::json!({
        "HOME": roots.0.join("caller-home"),
        "PATH": std::env::var("PATH").unwrap_or_default(),
    });

    server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "a", "cwd": roots.0.join("one"), "env": caller_env,
            "model": "codex/gpt-5.6-terra",
        }))
        .await
        .assert_status(201);
    server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "b", "cwd": roots.0.join("two"), "env": caller_env,
            "model": "codex/gpt-5.6",
            "spotter_model": "codex/gpt-5.6-luna",
            "triage": {"mode": "separate", "model": "codex/gpt-5.6-luna"},
        }))
        .await
        .assert_status(201);

    // The platform arm reads the same catalog under its own cache key, seeded
    // here so the creation makes no request; the key it holds stays out of the
    // response.
    crate::api::seed_codex_catalog(crate::api::Provider::OpenAI, "");
    let platform = server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "platform", "cwd": roots.0.join("two"), "env": caller_env,
            "model": "platform/gpt-5.6",
        }))
        .await;
    platform.assert_status(201);
    assert!(
        !platform
            .body()
            .to_string()
            .contains("platform-canary-secret")
    );

    let state_a = server.get("/sessions/a/v1/state").await;
    state_a.assert_status(200);
    let body_a = state_a.body().to_string();
    assert_eq!(
        json(&body_a)["session_identity"]["inference"],
        serde_json::json!({
            "main": {"connection": "codex", "provider": "openai-codex", "model": "gpt-5.6-terra"},
            "spotter": {"connection": "codex", "provider": "openai-codex", "model": "gpt-5.6-luna"},
            "triage": {"shared": true, "connection": "codex", "provider": "openai-codex", "model": "gpt-5.6-terra"},
        })
    );
    assert_eq!(json(&body_a)["session_identity"]["model"], "gpt-5.6-terra");
    let state_b = server.get("/sessions/b/v1/state").await;
    state_b.assert_status(200);
    let body_b = state_b.body().to_string();
    assert_eq!(
        json(&body_b)["session_identity"]["inference"],
        serde_json::json!({
            "main": {"connection": "codex", "provider": "openai-codex", "model": "gpt-5.6"},
            "spotter": {"connection": "codex", "provider": "openai-codex", "model": "gpt-5.6-luna"},
            "triage": {"shared": false, "connection": "codex", "provider": "openai-codex", "model": "gpt-5.6-luna"},
        })
    );

    // A triage on the Messages dialect cannot replay a Responses transcript: refused
    // at resolution, naming both roles, before a session dir exists.
    let refused = server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "c", "cwd": roots.0.join("two"), "env": caller_env,
            "triage": {"mode": "separate", "model": "messages/claude-haiku-4-5"},
        }))
        .await;
    refused
        .assert_status(400)
        .assert_body_contains("dialects differ");
    assert!(!roots.0.join("two/.efference/c").exists());
    // A connection that exists but is unconfigured is named as such, not resolved
    // without one.

    server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "d", "cwd": roots.0.join("two"), "env": caller_env,
            "model": "openrouter/anthropic/claude-sonnet-4-5",
        }))
        .await
        .assert_status(400)
        .assert_body_contains("not configured");

    // Canaries: no host credential in state, the log (which holds the child's
    // `env` output), or the session config. A secret that does reach a log by
    // another route is masked by name, without a value prefix — for every role
    // the session selected, and only those: session A never held the platform key.
    let log = |cwd: &str, id: &str| {
        std::fs::read_to_string(
            roots
                .0
                .join(cwd)
                .join(".efference")
                .join(id)
                .join(crate::boot::LOG_NAME),
        )
        .unwrap()
    };
    for haystack in [&body_a, &body_b, &log("one", "a"), &log("two", "b")] {
        for canary in [
            "codex-canary-token",
            "platform-canary-secret",
            "messages-canary-secret",
            "PLATFORM_KEY",
            "MESSAGES_KEY",
        ] {
            assert!(!haystack.contains(canary), "{canary} leaked:\n{haystack}");
        }
    }
    host.session("a")
        .unwrap()
        .append(1, user("token codex-canary-token"))
        .unwrap();
    // Session b's roles are all on the codex connection, so the platform key is
    // not among its secrets and a line naming one is *not* masked — masking is
    // per session, over the credentials its own roles resolved. The probe names
    // what b holds, and the haystack assertions above still hold every host
    // secret out of both state bodies and both logs.
    host.session("b")
        .unwrap()
        .append(1, user("token codex-canary-token and effort high"))
        .unwrap();
    let log_a = log("one", "a");
    let log_b = log("two", "b");
    assert!(
        log_a.contains("token ⟦codex (Codex cache): elided by harness⟧"),
        "{log_a}"
    );
    assert!(
        log_b.contains("token ⟦codex (Codex cache): elided by harness⟧ and effort high"),
        "{log_b}"
    );

    host.swansong().shut_down().await;
}

/// Each session's `/files` under its own prefix, in both spellings of a
/// directory: the wildcard capture the router hands this surface is the path
/// below the mount, so a listing has to come back under the whole prefix
/// without the route reconstructing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosted_sessions_files_are_served_under_its_prefix() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let runtime = SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong());
    host.adopt("one".into(), runtime);
    let host_state = Arc::clone(&host);
    let server = TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host_state);
            async move { info.with_shared_state(host) }
        }),
        crate::daemon::host::host_routes(true),
    ))
    .await;

    let listing = server.get("/sessions/one/files/").await;
    listing.assert_status(200);
    let names: Vec<String> = json(listing.body())["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"scratch".to_string()), "{names:?}");

    for path in [
        "/sessions/one/files/scratch",
        "/sessions/one/files/scratch/",
    ] {
        let conn = server.get(path).await;
        conn.assert_status(200);
        assert_eq!(json(conn.body())["path"], "scratch", "{path}");
    }

    server
        .get("/sessions/one/files/nowhere.txt")
        .await
        .assert_status(404);
}

/// The accepted temperature is the resolver's rule, so it holds at creation's
/// HTTP boundary too: a value `u8` accepts but the range does not is refused
/// there, not left for the first request to the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_enforces_the_temperature_range() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    for (temperature, status) in [(200u16, 400), (260, 400)] {
        server
            .post("/sessions")
            .with_json_body(&serde_json::json!({
                "cwd": roots.0.join("one"), "env": {}, "temperature": temperature,
            }))
            .await
            .assert_status(status);
    }
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}

/// `GET /inference/catalog` and `/inference/catalog/:connection`
/// (docs/model-catalog.md): a connection that cannot be read reports why without
/// a provider read, and a recent pick for it still arrives as a bare row. Only a
/// disabled connection is configured, so no refresh leaves the process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_reports_an_unavailable_connection_with_its_recent_picks() {
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[connections.off]
provider = "openrouter"
auth = { kind = "env", key = "TEST_KEY" }
disabled = true
"#,
    )
    .unwrap();
    let env = crate::env_table::EnvSnapshot::new(
        [("TEST_KEY".to_string(), "test-secret".to_string())].into(),
    )
    .unwrap();
    let history = crate::history::History::at(roots.0.join("state.db"));
    history.record(&crate::history::Pick {
        at: crate::history::now(),
        cwd: cwd.to_string_lossy().into_owned(),
        config_key: crate::history::BUILTIN_CONFIG_KEY.to_string(),
        connection: "off".into(),
        provider: "openrouter".into(),
        model: "z-ai/glm-5.3".into(),
    });
    let host = Host::new(
        crate::inference::InferenceRegistry::from_config(
            config,
            Path::new("/host"),
            env,
            &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
        )
        .unwrap()
        .with_history(history),
        baseline_env(),
    );
    let client = host_client(&host, "one").await.with_prefix("");
    let all = client.catalog(&cwd, None).await.expect("read the catalog");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, crate::client::CatalogStatus::Unavailable);
    assert!(all[0].error.as_deref().unwrap().contains("disabled"));
    assert_eq!(
        serde_json::to_string(&all[0].models).unwrap(),
        r#"[{"connection":"off","model":"z-ai/glm-5.3","group":"z-ai","recent":1}]"#
    );
    let one = client
        .catalog_connection("off", &cwd, None, Some(300))
        .await
        .expect("read one connection");
    assert_eq!(one, all[0]);
    assert!(
        client
            .catalog_connection("nope", &cwd, None, None)
            .await
            .is_err()
    );
}

/// A `:id` route keeps its own id under the host's `/sessions/<id>/…` prefix.
/// The wildcard that hands `/v1/…` to the session's router rewrites the *path*
/// but leaves the session segment in the conn's params, so a same-named capture
/// there would be read in place of the route's own — every `/v1/processes/:id`
/// answering `400 process id must be an integer` for a well-formed ordinal, and
/// the kill never reaching a process at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_id_survives_the_session_prefix() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let runtime = SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong());
    let session = Arc::clone(runtime.session());
    host.adopt("one".into(), runtime);

    let handle = session
        .spawn_background("sleep 30".into(), None)
        .expect("spawn");
    assert_eq!(handle, "p#1");

    let server = host_server(&host).await;

    let read = server.get("/sessions/one/v1/processes/1").await;
    read.assert_status(200);
    read.assert_body_contains(r#""id":1"#);

    let killed = server.delete("/sessions/one/v1/processes/1").await;
    killed.assert_status(200);
    killed.assert_body_contains(r#""status":"killed""#);

    // The signal landed: the drain saw the exit and cleared the running set.
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.running_process(1).is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the killed process left the running set");
}
