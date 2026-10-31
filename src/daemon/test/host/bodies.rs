//! The bytes the host's own routes answer with (docs/control-api.md), one pin per body:
//! the model catalog, the session listing, and the answers to creating, resuming,
//! supervising and archiving a session. The clients parse these, so a change to them is a
//! change to the control channel and fails here first.
//!
//! The comparison is of whole strings. A body built from values has its keys sorted at
//! every depth, and a body serialized from a type keeps the type's field order, so a pin
//! here also says which of the two a route does. The remembered locations, the preview,
//! and the worktree answers are pinned in `projects.rs`, `preview.rs` and `worktree.rs`.

use super::starting::endless_hook;
use super::*;
use crate::api::{ListedModel, ModelKind, Price};
use crate::history::{BUILTIN_CONFIG_KEY, History, Pick};
use crate::log::{Payload, Provider};

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

/// `body` with the one clock-dependent number a catalog entry carries, the seconds since
/// its listing was stored, set to 0.
fn without_age(body: &str) -> String {
    let Some(start) = body.find(r#""age":"#) else {
        return body.to_string();
    };
    let digits = start + r#""age":"#.len();
    let end = body[digits..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(body.len(), |len| digits + len);
    format!("{}0{}", &body[..digits], &body[end..])
}

/// The catalog list sorts its keys at every depth and its single-entry read keeps the
/// type's order. The listing is one stored copy with every field set, a decision model, a
/// row with nothing but its id, and a recent pick the copy does not list; a connection
/// that cannot be read has no age or rows of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_keeps_its_bytes() {
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    let state = roots.0.join("state.db");
    let history = History::at(state.clone());
    let pick = |model: &str, at: i64| Pick {
        at,
        cwd: cwd.to_string_lossy().into_owned(),
        config_key: BUILTIN_CONFIG_KEY.to_string(),
        connection: "test".into(),
        provider: "anthropic".into(),
        model: model.into(),
    };
    history.record(&pick("claude-x", crate::history::now()));
    history.record(&pick("typed-id", crate::history::now() - 86_400));
    let rows = vec![
        ListedModel {
            group: Some("anthropic".into()),
            name: Some("Claude X".into()),
            description: Some("The larger one.".into()),
            created: Some(1_754_000_000),
            context: Some(200_000),
            price: Some(Price {
                input: 3.0,
                output: 15.0,
            }),
            reasoning: Some(true),
            images: Some(false),
            line: Some("sonnet".into()),
            lifecycle: Some("active".into()),
            ..ListedModel::new("claude-x")
        },
        ListedModel {
            kind: ModelKind::Decisions,
            ..ListedModel::new("claude-d")
        },
        ListedModel::new("bare"),
    ];
    // The stored copy goes in through a handle on the file the host reads.
    crate::catalog::Catalog::new(Some(state))
        .refresh_with(
            "test",
            Provider::Anthropic,
            Duration::from_secs(60),
            || async { Ok(rows) },
        )
        .await;
    let host = Host::new(
        one_connection_registry().with_history(history),
        baseline_env(),
    );
    let server = host_server(&host).await;
    let query = format!("cwd={}", encode(&cwd));

    let all = server.get(&format!("/inference/catalog?{query}")).await;
    all.assert_status(200);
    assert_eq!(
        without_age(all.body()),
        concat!(
            r#"{"connections":[{"age":0,"connection":"test","models":["#,
            r#"{"connection":"test","context":200000,"created":1754000000,"description":"The larger one.","group":"anthropic","#,
            r#""images":false,"lifecycle":"active","line":"sonnet","model":"claude-x","name":"Claude X","#,
            r#""price":{"input":3.0,"output":15.0},"reasoning":true,"recent":1},"#,
            r#"{"connection":"test","kind":"decisions","model":"claude-d"},"#,
            r#"{"connection":"test","model":"bare"},"#,
            r#"{"connection":"test","model":"typed-id","recent":2}],"#,
            r#""provider":"anthropic","status":"fresh"}]}"#
        )
    );
    let one = server
        .get(&format!("/inference/catalog/test?{query}&role=main"))
        .await;
    one.assert_status(200);
    assert_eq!(
        without_age(one.body()),
        concat!(
            r#"{"connection":"test","provider":"anthropic","status":"fresh","age":0,"models":["#,
            r#"{"connection":"test","model":"claude-x","group":"anthropic","name":"Claude X","description":"The larger one.","#,
            r#""created":1754000000,"context":200000,"price":{"input":3.0,"output":15.0},"reasoning":true,"images":false,"#,
            r#""line":"sonnet","lifecycle":"active","recent":1},"#,
            r#"{"connection":"test","model":"bare"},"#,
            r#"{"connection":"test","model":"typed-id","recent":2}]}"#
        )
    );
    host.swansong().shut_down().await;
}

/// A connection that cannot be read: no age, and the reason as its `error`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_connection_keeps_its_bytes() {
    let roots = Roots::new();
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
    let host = Host::new(
        crate::inference::InferenceRegistry::from_config(
            config,
            Path::new("/host"),
            &env,
            &Arc::new(crate::secrets::Secrets::in_memory()),
        )
        .unwrap()
        .with_history(History::at(roots.0.join("state.db"))),
        baseline_env(),
    );
    let server = host_server(&host).await;

    let all = server.get("/inference/catalog").await;
    all.assert_status(200);
    let value = json(all.body());
    let reason = value["connections"][0]["error"].as_str().unwrap();
    assert_eq!(
        all.body(),
        format!(
            r#"{{"connections":[{{"connection":"off","error":{},"models":[],"provider":"openrouter","status":"unavailable"}}]}}"#,
            quoted(reason)
        )
    );
    let one = server.get("/inference/catalog/off").await;
    assert_eq!(
        one.body(),
        format!(
            r#"{{"connection":"off","provider":"openrouter","status":"unavailable","error":{},"models":[]}}"#,
            quoted(reason)
        )
    );
    host.swansong().shut_down().await;
}

/// The inventory of configured connections names no endpoint or credential, only whether
/// each can be used, with its keys sorted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_connection_inventory_keeps_its_bytes() {
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let inventory = server.get("/inference").await;
    inventory.assert_status(200);
    assert_eq!(
        inventory.body(),
        r#"{"connections":[{"available":true,"name":"test","provider":"anthropic"}]}"#
    );
    host.swansong().shut_down().await;
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

/// The listing's rows, in the type's field order: a starting session with no title or
/// processes yet, and a hosted one a session created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_listing_keeps_its_bytes() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");
    let _starting = host.reserve_starting("early", &cwd);
    host.adopt_with(
        "later".into(),
        runtime::started(runtime::boot(&cwd), host.swansong()).await,
        Some("early".into()),
        false,
    )
    .expect("the id is free in this test");
    let cwd = quoted(&cwd.to_string_lossy());

    let listing = server.get("/sessions").await;
    listing.assert_status(200);
    let rows = format!(
        r#"[{{"id":"early","cwd":{cwd},"status":"starting","processes":0}},{{"id":"later","cwd":{cwd},"created_by":"early","status":"idle","processes":0}}]"#
    );
    assert_eq!(listing.body(), rows);
    assert_eq!(
        server.get("/sessions/later").await.body(),
        format!(
            r#"{{"id":"later","cwd":{cwd},"created_by":"early","status":"idle","processes":0}}"#
        )
    );
    host.swansong().shut_down().await;
}

/// Creating a session answers its id, and a starting one is archived to `discarded`; a
/// resume answers whether this call reopened it; and archiving a resume in progress
/// leaves it `archived`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creating_and_resuming_a_session_keep_their_bytes() {
    let roots = Roots::new();
    let host = Host::new(
        registry_with_hook(&endless_hook()).with_history(History::at(roots.0.join("state.db"))),
        marked_baseline(),
    );
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");

    let created = server
        .post("/sessions")
        .with_json_body(&stops_at_boot(&cwd))
        .await;
    created.assert_status(201);
    assert_eq!(created.body(), r#"{"id":"fresh"}"#);
    let discarded = server.delete("/sessions/fresh").await;
    discarded.assert_status(200);
    assert_eq!(discarded.body(), r#"{"id":"fresh","state":"discarded"}"#);

    archived::write_session(&cwd, "dormant", "Dormant work");
    let resumed = server
        .patch("/sessions/dormant")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": cwd}))
        .await;
    resumed.assert_status(200);
    assert_eq!(
        resumed.body(),
        r#"{"id":"dormant","resumed":true,"state":"active"}"#
    );
    let again = server
        .patch("/sessions/dormant")
        .with_json_body(&serde_json::json!({"state": "active", "cwd": cwd}))
        .await;
    again.assert_status(200);
    assert_eq!(
        again.body(),
        r#"{"id":"dormant","resumed":false,"state":"active"}"#
    );
    let archived = server.delete("/sessions/dormant").await;
    archived.assert_status(200);
    assert_eq!(archived.body(), r#"{"id":"dormant","state":"archived"}"#);

    host.swansong().shut_down().await;
}

/// Setting and clearing a supervisor answers the pointer as it now reads, `null` when it
/// is clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_answer_keeps_its_bytes() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    for id in ["a", "b"] {
        let session_dir = roots.0.join(".efference").join(id);
        std::fs::create_dir_all(&session_dir).expect("the session dir exists");
        host.adopt(
            id.into(),
            runtime::started(runtime::boot_in(&roots.0, &session_dir), host.swansong()).await,
        )
        .expect("the id is free in this test");
    }
    let server = host_server(&host).await;

    let set = server
        .patch("/sessions/a")
        .with_json_body(&serde_json::json!({ "supervisor": "b" }))
        .await;
    set.assert_status(200);
    assert_eq!(
        set.body(),
        r#"{"id":"a","state":"active","supervisor":"b"}"#
    );
    let cleared = server
        .patch("/sessions/a")
        .with_json_body(&serde_json::json!({ "supervisor": null }))
        .await;
    cleared.assert_status(200);
    assert_eq!(
        cleared.body(),
        r#"{"id":"a","state":"active","supervisor":null}"#
    );

    host.swansong().shut_down().await;
}

/// Archiving answers the state it reached; `DELETE` names the sessions that ended with
/// the one archived, and a patch to `archived` says in `resumed` that it reopened nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_answer_keeps_its_bytes() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let cwd = roots.0.join("one");
    for (id, created_by) in [("parent", None), ("child", Some("parent")), ("other", None)] {
        let session_dir = cwd.join(".efference").join(id);
        std::fs::create_dir_all(&session_dir).expect("the session dir exists");
        host.adopt_with(
            id.into(),
            runtime::started(runtime::boot_in(&cwd, &session_dir), host.swansong()).await,
            created_by.map(str::to_string),
            false,
        )
        .expect("the id is free in this test");
        // A session with a turn is archived; an empty one is discarded instead.
        host.session(id)
            .unwrap()
            .append(1, Payload::User(vec![]))
            .unwrap();
    }

    let ended = server.delete("/sessions/parent").await;
    ended.assert_status(200);
    assert_eq!(
        ended.body(),
        r#"{"ended_with":["child"],"id":"parent","state":"archived"}"#
    );
    let patched = server
        .patch("/sessions/other")
        .with_json_body(&serde_json::json!({"state": "archived", "source": "web"}))
        .await;
    patched.assert_status(200);
    assert_eq!(
        patched.body(),
        r#"{"id":"other","resumed":false,"state":"archived"}"#
    );

    host.swansong().shut_down().await;
}
