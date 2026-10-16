//! Tests for the advertised tool surface and the loop-owned tools.
use super::*;

/// `tool_catalog` covers every name `tool_definitions` advertises — the gap
/// review found: a catalog built from `Toolbox::tool_catalog` alone answered
/// only the mounted providers, so `GET /v1/tools/check_inbox` 404'd for a
/// tool the session's own `tools[]` carried.
#[test]
fn tool_catalog_covers_every_advertised_name() {
    let toolbox = Toolbox::bash_only();
    let advertised: std::collections::HashSet<String> = tool_definitions(&toolbox)
        .iter()
        .filter_map(|d| d.get("name")?.as_str().map(str::to_string))
        .collect();
    let catalog = tool_catalog(&toolbox);
    let cataloged: std::collections::HashSet<String> = catalog.keys().cloned().collect();
    assert_eq!(
        advertised, cataloged,
        "every wire name in tools[] has a catalog entry, and vice versa"
    );
}

/// A `Toolbox`-mounted tool's served schema carries the `intent` `with_intent`
/// injected on the real wire; a loop-owned tool's does not — `tool_definitions`
/// pushes `consolidate`/the process monitor family/the triage tools onto the array
/// *after* `with_intent` already ran over the toolbox half, so the model never
/// saw `intent` declared on any of them.
#[test]
fn only_the_toolbox_half_of_the_catalog_carries_injected_intent() {
    let toolbox = Toolbox::bash_only();
    let catalog = tool_catalog(&toolbox);

    let bash = &catalog[bash::NAME];
    assert!(
        bash.input_schema["properties"]["intent"].is_object(),
        "a toolbox-mounted tool's served schema matches the wire, intent included"
    );

    for name in [
        consolidate::NAME,
        process_monitor::CHECK_INBOX,
        crate::triage::retain_tool()["name"].as_str().unwrap(),
        crate::triage::triage_tool()["name"].as_str().unwrap(),
    ] {
        let entry = &catalog[name];
        assert!(
            entry.server.is_none(),
            "{name} is the harness's own, not a mounted server's"
        );
        assert!(
            entry
                .input_schema
                .get("properties")
                .is_none_or(|p| p.get("intent").is_none()),
            "{name} was never sent an injected intent on the wire: {entry:?}"
        );
    }
}

#[tokio::test]
async fn wait_refuses_when_no_known_source_can_wake_it() {
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let result = process_monitor_tool_result(&shared, &call(process_monitor::WAIT))
        .await
        .expect("wait is a process monitor tool");

    assert!(result.is_error, "a wait with no wake source must refuse");
    assert!(result.content.contains("nothing can wake"), "{result:?}");
    assert!(
        result.content.contains("pending notification"),
        "{result:?}"
    );
    assert!(
        result.content.contains("running background process"),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_wakes_for_a_notification_and_returns_its_body() {
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let handle = shared
        .spawn_background("sleep 1".into(), None)
        .expect("background process makes wait safe");
    let sender = Arc::clone(&shared);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        sender.notify("ci".into(), "build completed".into(), None, None);
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        process_monitor_tool_result(&shared, &call(process_monitor::WAIT)),
    )
    .await
    .expect("wait wakes when notify enqueues")
    .expect("wait is a process monitor tool");
    assert!(!result.is_error, "notification wakes wait: {result:?}");
    assert!(
        result.content.contains("ci · build completed"),
        "{result:?}"
    );
    assert!(shared.kill_bg(&handle), "clean up the test process");
}

#[test]
pub(super) fn cap_passes_through_results_under_the_limit() {
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let r = RawResult::text("a".repeat(MAX_TOOL_RESULT_BYTES));
    let capped = cap_tool_result(&shared, &call("search"), r);
    assert!(!capped.is_error);
    assert_eq!(capped.content.len(), MAX_TOOL_RESULT_BYTES);
}

#[test]
pub(super) fn cap_turns_oversized_results_into_a_hard_error_with_a_head() {
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let huge = "x".repeat(MAX_TOOL_RESULT_BYTES + 1);
    let capped = cap_tool_result(&shared, &call("search"), RawResult::text(huge));
    assert!(capped.is_error, "over-cap result must be a hard error");
    // The result carries the explanation + head, not the megabytes.
    assert!(
        capped.content.len() < 4096,
        "the error is small, not the raw payload"
    );
    assert!(
        capped.content.contains("search"),
        "names the offending tool"
    );
    assert!(
        capped.content.contains("over the"),
        "states it crossed the limit"
    );
}

/// The head is cut on a char boundary, never mid-codepoint (it would panic / produce
/// invalid UTF-8 otherwise).
#[test]
pub(super) fn cap_head_is_char_boundary_safe() {
    // 4-byte chars straddling the 1KB head boundary.
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let huge = "🦀".repeat(MAX_TOOL_RESULT_BYTES); // 4 bytes each → well over the cap
    let capped = cap_tool_result(&shared, &call("read"), RawResult::text(huge));
    assert!(capped.is_error);
    // Round-trips as valid UTF-8 (the test would not even construct otherwise, but
    // assert the head contains whole crabs).
    assert!(capped.content.contains('🦀'));
}

/// The callable set is read off the turn's own `tools[]`, minus the triage tools — which
/// stand in every array for cache-lineage reasons but are never asked for on an agent
/// turn (docs/triage.md). Reading the same array is what keeps the check from
/// rejecting a tool the model was just shown.
#[test]
pub(super) fn callable_names_are_the_advertised_ones_minus_the_triage_tools() {
    let toolbox = Toolbox::bash_only();
    let callable = super::callable_names(&toolbox);
    for advertised in [crate::bash::NAME, crate::consolidate::NAME] {
        assert!(
            callable.iter().any(|n| n == advertised),
            "{advertised} is advertised, so a call may name it: {callable:?}"
        );
    }
    for triage in tool_definitions(&toolbox)
        .iter()
        .filter_map(|d| d.get("name")?.as_str())
        .filter(|n| is_triage_tool(n))
    {
        assert!(
            !callable.iter().any(|n| n == triage),
            "{triage} is advertised but never asked for on an agent turn"
        );
    }
}
