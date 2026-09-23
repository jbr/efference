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
/// A host registry with one env-backed connection, so creation gets past role
/// resolution and into `boot::prepare`. The credential never reaches a provider:
/// every case here fails before the first request.
fn one_connection_registry() -> crate::inference::InferenceRegistry {
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[defaults]
model = "test/m"

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

/// `QUERY /sessions` (docs/control-api.md): the host's preview of a proposed
/// session — the effective selections, where each came from, and the issues that
/// would stop creation — without minting anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_previews_a_proposed_session_without_creating_one() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry());
    let client = host_client(&host, "one").await.with_prefix("");
    let preview = client
        .session_options(&SessionOptionsRequest::new(roots.0.join("one")))
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
                    "source": "user_default",
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
    let host = Host::new(one_connection_registry());
    let server = host_server(&host, true).await;
    let response = server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!({
            "cwd": roots.0.join("one"),
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
    let host = Host::new(one_connection_registry());
    let server = host_server(&host, true).await;
    server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!(["not", "an", "object"]))
        .await
        .assert_status(400);
    host.swansong().shut_down().await;
}

/// Preview mints nothing and reads no environment, so it is not a network
/// boundary: a listener without the Unix creation capability still answers it.
/// Creation itself still refuses, for want of an environment recipe rather than
/// for a security reason (backlog/host-process.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_network_host_previews_but_does_not_create() {
    let host = Host::new(one_connection_registry());
    let server = host_server(&host, false).await;
    let response = server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!({"cwd": "/project"}))
        .await;
    response.assert_status(200);
    assert_eq!(
        json(response.body())["effective"]["main"]["value"]["model"],
        "m"
    );
    assert!(host.ids().is_empty());
    server
        .post("/sessions")
        .with_json_body(&serde_json::json!({"cwd": "/project", "env": {}}))
        .await
        .assert_status(403);
    host.swansong().shut_down().await;
}

/// `GET /inference` is a host inventory, never a projection of its credential
/// sources or their values. Rows are sorted by name, and a disabled connection is
/// listed as unavailable rather than omitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inference_inventory_is_sorted_and_secret_free() {
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[defaults]
model = "test/m"

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
    let host = Host::new(crate::inference::InferenceRegistry::empty());
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
    let host = Host::new(crate::inference::InferenceRegistry::empty());
    for label in ["one", "two"] {
        let runtime = SessionRuntime::start(runtime::boot(&roots.0.join(label)), host.swansong());
        host.adopt(label.into(), runtime);
    }
    let server = host_server(&host, true).await;

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
    let host = Host::new(crate::inference::InferenceRegistry::empty());
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
    let host = Host::new(one_connection_registry());
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
    );
    let server = host_server(&host, true).await;
    let base = |name: &str, cwd: &Path| {
        serde_json::json!({
            "name": name, "cwd": cwd, "model": "m",
            "env": {"HOME": "/h"},
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
[defaults]
model = "codex/gpt-5.6-terra"

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
    let registry = crate::inference::InferenceRegistry::load(
        &roots.0,
        host_env,
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .expect("host registry");
    let host = Host::new(registry);
    let server = host_server(&host, true).await;

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
        }))
        .await
        .assert_status(201);
    server
        .post("/sessions")
        .with_json_body(&serde_json::json!({
            "name": "b", "cwd": roots.0.join("two"), "env": caller_env,
            "model": "platform/gpt-5.6",
            "spotter_model": "codex/gpt-5.6-luna",
            "triage": {"mode": "separate", "model": "platform/gpt-5.6-luna"},
        }))
        .await
        .assert_status(201);

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
            "main": {"connection": "platform", "provider": "openai", "model": "gpt-5.6"},
            "spotter": {"connection": "codex", "provider": "openai-codex", "model": "gpt-5.6-luna"},
            "triage": {"shared": false, "connection": "platform", "provider": "openai", "model": "gpt-5.6-luna"},
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
    // on the default connection.
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
    host.session("b")
        .unwrap()
        .append(
            1,
            user("token codex-canary-token and key platform-canary-secret"),
        )
        .unwrap();
    let log_a = log("one", "a");
    let log_b = log("two", "b");
    assert!(
        log_a.contains("token ⟦codex (Codex cache): elided by harness⟧"),
        "{log_a}"
    );
    assert!(
        log_b.contains(
            "token ⟦codex (Codex cache): elided by harness⟧ and key \
             ⟦platform (PLATFORM_KEY): elided by harness⟧"
        ),
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
    let host = Host::new(crate::inference::InferenceRegistry::empty());
    let runtime = SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong());
    host.adopt("one".into(), runtime);
    let host_state = Arc::clone(&host);
    let server = TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host_state);
            async move { info.with_shared_state(host) }
        }),
        crate::daemon::host::host_routes(true, true),
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
    let host = Host::new(one_connection_registry());
    let server = host_server(&host, true).await;
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
