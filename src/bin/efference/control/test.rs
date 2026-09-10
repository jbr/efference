//! The control tools, exercised over the real control router (docs/operating.md).

use super::*;
use efference::persist::Log;
use mcplease::traits::AsToolsList;
use mcplease::types::{JsonRpcRequest, JsonRpcResponse};
use std::sync::Arc;

/// A [`Control`] whose default session is a live [`router`](efference::daemon::router)
/// on `trillium_testing`'s in-memory transport — the whole request/response cycle,
/// with no socket bound (`daemon/test.rs` explains why the tests never bind one).
async fn test_state(cli: bool) -> Control {
    let (shared, _turns) = efference::daemon::Session::new(Log::in_memory());
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared);
            async move { info.with_shared_state(shared) }
        }),
        efference::daemon::handler(
            efference::daemon::Transport::Socket { auth: None },
            efference::daemon::App::session(),
        ),
    );
    let context = trillium_testing::init(&mut handler).await;
    Control::with_client(
        Handle::current(),
        Client::with_connector(
            trillium_testing::ServerConnector::new(handler).with_context(context),
        ),
        cli,
    )
}

/// One `tools/call` end to end: the JSON-RPC a mounted harness sends, through
/// mcplease's dispatch, through the synchronous `execute`, through a real
/// `GET /state`, and back out as both content halves.
///
/// Dispatch is synchronous and blocks on the runtime, so it runs on a blocking
/// thread — the same shape `run_cli`/`run_stdio` use, and the reason a tool may never
/// be called from a runtime worker.
#[tokio::test(flavor = "multi_thread")]
async fn tools_call_state_reaches_the_daemon() {
    let mut state = test_state(false).await;
    let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "state", "arguments": {} }
    }))
    .unwrap();
    let config = mcplease::ServerConfig::new(mcplease::server_info!());

    let response = tokio::task::spawn_blocking(move || {
        mcplease::handle_request::<Tools, Control>(request, &mut state, &config)
    })
    .await
    .unwrap();

    let JsonRpcResponse { result, error, .. } = response;
    assert!(error.is_none(), "{error:?}");
    let result = result.expect("a result");
    assert_ne!(
        result.get("isError"),
        Some(&serde_json::Value::Bool(true)),
        "the tool call failed: {result}"
    );
    // The program-facing half is the typed `StateResult`: no `settled` (nothing
    // waited), and a `state` object that is the daemon's own `GET /state` body.
    let structured = result
        .get("structuredContent")
        .expect("state is a structured_output! tool");
    assert!(structured.get("settled").is_none());
    let daemon_state = structured.get("state").expect("the /state body");
    assert!(
        daemon_state.get("review_level").is_some() && daemon_state.get("pace").is_some(),
        "not a /state body: {daemon_state}"
    );
    // The model-facing half is the same value as text.
    let text = result["content"][0]["text"].as_str().expect("a text block");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).unwrap(),
        *structured
    );
}

/// The advertised list is what a mounting harness caches, so both its **order** and
/// its behaviour hints are pinned here. Order is macro-expansion order; a reorder
/// moves every client's cached list. The hints are what an approval UX shows, and
/// the spec's defaults are pessimistic, so an unset one reads as a destructive
/// open-world mutation.
#[test]
fn tools_list_is_ordered_and_annotated() {
    let tools = Tools::tools_list();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "turn",
            "approve",
            "settle",
            "interrupt",
            "background",
            "pace",
            "review_level",
            "notify",
            "dismiss",
            "image",
            "state",
            "log",
        ]
    );

    let hints = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} declares no annotations"));
        (
            annotations.read_only_hint,
            annotations.destructive_hint,
            annotations.idempotent_hint,
        )
    };

    for name in ["state", "log", "settle", "image"] {
        assert_eq!(hints(name).0, Some(true), "{name} is a read");
    }
    // Interrupt kills a process group; approving runs a call the rungs did not clear.
    for name in ["interrupt", "approve"] {
        assert_eq!(
            hints(name),
            (Some(false), Some(true), Some(false)),
            "{name}"
        );
    }
    for name in ["turn", "background", "pace", "notify"] {
        assert_eq!(
            hints(name),
            (Some(false), Some(false), Some(false)),
            "{name} is a non-idempotent write"
        );
    }
    // Every tool declares a closed world: they reach one local daemon, nothing else.
    for tool in &tools {
        assert_eq!(
            tool.annotations.as_ref().unwrap().open_world_hint,
            Some(false),
            "{}",
            tool.name
        );
    }
}
