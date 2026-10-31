//! One process hosting several sessions behind `/sessions/<id>/…` (docs/control-channel.md).

use super::*;
use crate::daemon::{Host, SessionRuntime};
use serde_json::Value;
use std::path::Path;
use trillium::KnownHeaderName;

mod archived;
mod artifacts;
mod attention;
mod boot_resume;
mod control;
mod creation;
mod discard;
mod files;
mod island;
mod preview;
mod projects;
mod resume;
mod shell;
mod starting;
mod supervise;
mod terminal_title;
mod update_check;
mod worktree;

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
        &env,
        &Arc::new(crate::secrets::Secrets::in_memory()),
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
        &env,
        &Arc::new(crate::secrets::Secrets::in_memory()),
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
    use crate::auth::Root;
    use crate::devices::{Joined, Kind};
    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let root = std::env::temp_dir().join(format!("efference-net-host-{nonce:016x}"));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (auth, _listener) = tcp::arm_with_state(
        TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            public_url: "http://localhost:7777".into(),
            root: Root::new(root.to_string_lossy().as_ref()),
            open: false,
        },
        root.join("state.db"),
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
    let enrolled = auth
        .enroll(Kind::Script, "test", Joined::Add)
        .expect("a scratch database takes a row");
    (server, format!("Bearer {}", enrolled.token))
}

/// A hosted session's control answers under its prefix, over the web transport:
/// `no-store` names the inner route a session-prefixed path counts as, the
/// browsing-context isolation rides every answer, and the plaintext web
/// transport sends no HSTS (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosted_sessions_control_answers_are_never_stored() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let runtime = SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong());
    host.adopt("one".into(), runtime)
        .expect("the id is free in this test");
    let (server, bearer) = network_host_server(&host).await;

    let conn = server
        .get("/sessions/one/v1/state")
        .with_request_header("authorization", bearer)
        .await;
    conn.assert_ok();
    assert_eq!(
        conn.response_headers().get_str("cache-control"),
        Some("no-store"),
        "a session-prefixed control answer is its inner route's"
    );
    assert_eq!(
        conn.response_headers()
            .get_str("cross-origin-opener-policy"),
        Some("same-origin")
    );
    assert!(
        conn.response_headers()
            .get_str("strict-transport-security")
            .is_none(),
        "the plaintext web transport sends no HSTS"
    );
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

/// Post a creation, which answers once the id is reserved, and read where its
/// preparation ended from the session's own `/state?wait=settled`, as a client would:
/// a started session's state, or the starting state carrying the failure
/// (docs/control-api.md).
async fn created_state(server: &TestServer<impl trillium::Handler>, body: &Value) -> Value {
    let created = server.post("/sessions").with_json_body(body).await;
    created.assert_status(201);
    let id = json(created.body())["id"]
        .as_str()
        .expect("a creation answers its id")
        .to_string();
    settled_state(server, &id).await
}

/// `GET /sessions/<id>/v1/state?wait=settled`.
async fn settled_state(server: &TestServer<impl trillium::Handler>, id: &str) -> Value {
    let state = server
        .get(&format!("/sessions/{id}/v1/state?wait=settled"))
        .await;
    state.assert_status(200);
    json(state.body())
}

/// The reason a starting session's preparation failed, from its state; empty when
/// it did not fail.
fn failure_reason(state: &Value) -> String {
    state["starting"]["failure"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
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
    crate::api::seed_codex_catalog(crate::log::Provider::OpenAICodex, "acct-1");
    let registry = crate::inference::InferenceRegistry::load(
        &roots.0,
        &host_env,
        &Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .expect("host registry");
    let host = Host::new(registry, baseline_env());
    let server = host_server(&host).await;

    // Each project's `[context]` runs `env` in a tool child, so a host secret that
    // leaked into the child environment would land — masked — in the boot preamble.
    for label in ["one", "two"] {
        std::fs::write(
            roots.0.join(label).join("efference.toml"),
            "scope = 'test'\n[tools]\nefference = { enabled = true }\n'efference.read' = { granted = true }\n[paths]\n'.' = true\n[context]\nChildEnv = { exec = \"env\" }\n",
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
    crate::api::seed_codex_catalog(crate::log::Provider::OpenAI, "");
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

    // Settled, so through the start: creation answers once the id is reserved.
    let state_a = server.get("/sessions/a/v1/state?wait=settled").await;
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
    let state_b = server.get("/sessions/b/v1/state?wait=settled").await;
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
                .join(crate::session_dir::LOG_NAME),
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

/// `?role=` narrows the catalog to what a role may choose (docs/model-catalog.md):
/// a connection that serves no turns drops out for main and triage and stays for
/// the spotter. Both connections are disabled, so no refresh leaves the process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_role_filter_drops_decision_only_connections_for_turn_roles() {
    use crate::client::ModelRole;
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    let config = crate::inference::InferenceConfig::from_toml(
        r#"
[connections.text]
provider = "openrouter"
auth = { kind = "env", key = "TEST_KEY" }
disabled = true

[connections.decisions]
provider = "typesafe"
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
            &env,
            &Arc::new(crate::secrets::Secrets::in_memory()),
        )
        .unwrap(),
        baseline_env(),
    );
    let client = host_client(&host, "one").await.with_prefix("");
    let names = |entries: Vec<crate::client::ConnectionCatalog>| -> Vec<String> {
        let mut names: Vec<String> = entries.into_iter().map(|e| e.connection).collect();
        names.sort();
        names
    };
    let all = client.catalog(&cwd, None, None).await.unwrap();
    assert_eq!(names(all), ["decisions", "text"]);
    for role in [ModelRole::Main, ModelRole::Triage] {
        let turns = client.catalog(&cwd, None, Some(role)).await.unwrap();
        assert_eq!(names(turns), ["text"], "{role:?}");
    }
    let spotter = client
        .catalog(&cwd, None, Some(ModelRole::Spotter))
        .await
        .unwrap();
    assert_eq!(names(spotter), ["decisions", "text"]);

    // One connection: a role it cannot serve is refused by name, one it can is read.
    let refused = client
        .catalog_connection("decisions", &cwd, None, None, Some(ModelRole::Main))
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("serves no turns"), "{refused}");
    client
        .catalog_connection("decisions", &cwd, None, None, Some(ModelRole::Spotter))
        .await
        .expect("the spotter may use a decision-only connection");
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
            &env,
            &Arc::new(crate::secrets::Secrets::in_memory()),
        )
        .unwrap()
        .with_history(history),
        baseline_env(),
    );
    let client = host_client(&host, "one").await.with_prefix("");
    let all = client
        .catalog(&cwd, None, None)
        .await
        .expect("read the catalog");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, crate::client::CatalogStatus::Unavailable);
    assert!(all[0].error.as_deref().unwrap().contains("disabled"));
    assert_eq!(
        serde_json::to_string(&all[0].models).unwrap(),
        r#"[{"connection":"off","model":"z-ai/glm-5.3","group":"z-ai","recent":1}]"#
    );
    let one = client
        .catalog_connection("off", &cwd, None, Some(300), None)
        .await
        .expect("read one connection");
    assert_eq!(one, all[0]);
    assert!(
        client
            .catalog_connection("nope", &cwd, None, None, None)
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
    host.adopt("one".into(), runtime)
        .expect("the id is free in this test");

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
            &env,
            &Arc::new(crate::secrets::Secrets::in_memory()),
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
/// with no cursor and nothing to reconcile (`host/listing.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_listing_streams_its_current_value() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
    )
    .expect("the id is free in this test");
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
    )
    .expect("the id is free in this test");
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
        host.adopt(label.into(), runtime)
            .expect("the id is free in this test");
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
    host.adopt("one".into(), runtime)
        .expect("the id is free in this test");
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
