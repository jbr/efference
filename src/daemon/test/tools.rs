//! Tests for `GET /v1/tools/:name` (`http.rs`, docs/control-api.md).

use super::*;

/// A built-in's full definition answers, even though `Toolbox::brief` has no
/// entry for `bash` (that hole is a spotter concern — docs/permissions.md —
/// not an absence of the definition this route serves).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_built_in_tools_full_definition_is_present() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.set_tool_catalog(&crate::tools::Toolbox::bash_only());
    let app = test_server(&shared).await;

    let response = app.get("/v1/tools/bash").await;
    response.assert_status(200);
    let entry: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(entry["name"], "bash");
    assert!(entry["description"].as_str().unwrap().contains("bash"));
    assert!(
        entry["input_schema"]["properties"]["command"].is_object(),
        "the declared schema rides along: {entry}"
    );
    assert_eq!(
        entry["server"],
        Value::Null,
        "a built-in has no mounting server"
    );
}

/// A loop-owned tool — `check_inbox`, dispatched structurally by the loop,
/// outside `Toolbox` — answers too: the catalog `set_tool_catalog` snapshots
/// is `agent::tool_catalog`, which mirrors `agent::tool_definitions`'s full
/// union (the toolbox plus `consolidate`/the supervisor family/the peek
/// tools), not just the toolbox's own providers. This is the coverage gap
/// `agent::brief`'s doc comment already records for the spotter, pinned here
/// for the route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loop_owned_tools_full_definition_is_present() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.set_tool_catalog(&crate::tools::Toolbox::bash_only());
    let app = test_server(&shared).await;

    let response = app.get("/v1/tools/check_inbox").await;
    response.assert_status(200);
    let entry: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(entry["name"], "check_inbox");
    assert!(
        entry["description"]
            .as_str()
            .unwrap()
            .contains("notifications")
    );
    assert_eq!(
        entry["server"],
        Value::Null,
        "a harness-owned tool has no mounting server"
    );
    // Unlike a `Toolbox`-mounted tool, the loop-owned family is advertised
    // without `intent` injected (`agent::tool_definitions` pushes these onto
    // the array *after* `with_intent` already ran over the toolbox half), so
    // the served schema must not claim a property the wire never carried.
    assert!(
        entry["input_schema"]["properties"].get("intent").is_none(),
        "check_inbox declares no properties at all, let alone an injected intent: {entry}"
    );
}

/// An unknown wire name is a clean `404`, and so is any name at all when no
/// catalog was ever set — the "session read with no daemon behind it"
/// fallback the route treats as absence, not as an error state of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tool_names_are_404() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.set_tool_catalog(&crate::tools::Toolbox::bash_only());
    let app = test_server(&shared).await;
    app.get("/v1/tools/nope").await.assert_status(404);

    // No catalog set at all: every name, including a real one, is absent —
    // the "session read with no daemon behind it" fallback, not a distinct
    // error state.
    let (bare, _turns_rx) = Shared::new(Log::in_memory());
    let app = test_server(&bare).await;
    app.get("/v1/tools/bash").await.assert_status(404);
}
