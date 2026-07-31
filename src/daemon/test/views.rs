//! Tests for `views.rs`.

use super::*;

/// `GET /state` exposes the optional last upstream failure without putting a
/// transport diagnostic into the log (docs/transport.md).
#[test]
fn state_reports_upstream_failure() {
    let failure = crate::api::upstream_failure_state();
    let (shared, _turns_rx) = Shared::with_role_scratch_and_upstream_failure(
        Log::in_memory(),
        LoadedConfig::default(),
        None,
        Arc::clone(&failure),
    );

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["upstream_failure"].is_null());

    *failure.lock().unwrap() = Some(crate::api::UpstreamFailure {
        status: 429,
        count: 2,
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["upstream_failure"],
        serde_json::json!({ "status": 429, "count": 2 })
    );
}

/// `GET /state` reports cache telemetry: the latest `usage`, running totals,
/// and a seconds-since stamp. Drives the cache gate signature through it (docs/README.md) —
/// a cold first request (all creation) then a hot one (reads climb, creation
/// collapses) — and asserts the surface reflects both. Transport-only: the
/// log is untouched, so this never perturbs projection or byte-stability.
#[test]
fn state_reports_cache_telemetry() {
    let log_path = std::env::temp_dir().join("efference-usage-test.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));

    // No request yet: no `last`, no stamp.
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["usage"]["last"].is_null());
    assert!(v["usage"]["seconds_since_last_request"].is_null());

    // Cold prefix: all creation, no read.
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage {
            id: None,
            usage: Usage {
                input_tokens: 6,
                output_tokens: 10,
                cache_creation_input_tokens: 9407,
                cache_read_input_tokens: 0,
            },
        },
    );
    // Now hot: reads climb, creation collapses (the cache gate signature, docs/README.md).
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage {
            id: None,
            usage: Usage {
                input_tokens: 6,
                output_tokens: 12,
                cache_creation_input_tokens: 165,
                cache_read_input_tokens: 9407,
            },
        },
    );

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    // `last` is the most recent request.
    assert_eq!(v["usage"]["last"]["cache_read_input_tokens"], 9407);
    assert_eq!(v["usage"]["last"]["cache_creation_input_tokens"], 165);
    // Cumulative sums across both.
    assert_eq!(
        v["usage"]["cumulative"]["cache_creation_input_tokens"],
        9407 + 165
    );
    assert_eq!(v["usage"]["cumulative"]["cache_read_input_tokens"], 9407);
    assert_eq!(v["usage"]["cumulative"]["output_tokens"], 22);
    // A wall-clock stamp now exists — the eviction-prediction signal (docs/log-and-projection.md).
    assert!(v["usage"]["seconds_since_last_request"].is_u64());
}

/// `GET /state` reports the resolved role (docs/permissions.md): a
/// `null` source for the built-in default, and the path plus the **effective** config's full
/// contents (scope, watch-for, commands, tools, paths) when a config is in effect — so "what is
/// my live perimeter?" is answerable, not just "which file?". The contents track the *effective*
/// policy, so a folded config edit shows up (docs/permissions.md).
#[test]
fn state_reports_effective_role_contents() {
    // Default role (no config file) → null source, empty everywhere.
    let (shared, _rx) = Shared::new(Log::in_memory());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["role"]["source"].is_null());
    assert!(v["role"]["scope"].is_null());
    assert_eq!(v["role"]["commands"], serde_json::json!([]));

    // A loaded config surfaces its resolved path and full contents (sorted, by BTree order).
    let loaded = LoadedConfig {
        role: crate::config::Config {
            scope: Some("stay in the repo".into()),
            watch_for: vec!["network egress".into()],
            commands: ["cargo", "git diff"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            tools: ["read"].iter().map(|s| s.to_string()).collect(),
            paths: [(
                ".".to_string(),
                crate::config::Perms {
                    read: true,
                    write: true,
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        source: ConfigSource::Path("efference.toml".into()),
    };
    let (shared, _rx) = Shared::with_role(Log::in_memory(), loaded);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["role"]["source"], "efference.toml");
    assert_eq!(v["role"]["scope"], "stay in the repo");
    assert_eq!(
        v["role"]["watch_for"],
        serde_json::json!(["network egress"])
    );
    assert_eq!(
        v["role"]["commands"],
        serde_json::json!(["cargo", "git diff"])
    );
    assert_eq!(v["role"]["tools"], serde_json::json!(["read"]));
    assert_eq!(
        v["role"]["paths"]["."],
        serde_json::json!({ "read": true, "write": true })
    );

    // The fix: a *committed* config edit moves the reported perimeter (the boot config did not
    // list `npm`; after the edit, `/state` does) — `/state` reflects effective, not base.
    shared.append(0, allow_grant("npm")).unwrap();
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["role"]["commands"],
        serde_json::json!(["cargo", "git diff", "npm"])
    );
}

/// Autonomous mode defaults off and `/state` reports it both ways — the orchestrator-above
/// reads this to know whether a subordinate runs unsupervised (the autonomous flip).
#[test]
fn state_reports_autonomous_mode() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    assert!(!shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["autonomous"], false);

    shared.set_autonomous(true);
    assert!(shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["autonomous"], true);
}

/// The review-level dial (docs/permissions.md) is runtime-settable and
/// reported on `/state`: default `spotter_human`, and `autonomous` is a *derived* mirror of the
/// dial (true iff the level is `Autonomous`) — so the two stay consistent.
#[test]
fn state_reports_the_review_level_dial() {
    use crate::policy::ReviewLevel;
    let (shared, _rx) = Shared::new(Log::in_memory());

    // Default: supervised, not autonomous.
    assert_eq!(shared.review_level(), ReviewLevel::SpotterHuman);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "spotter_human");
    assert_eq!(v["autonomous"], false);

    // HumanOnly: reported, and still not autonomous (a human is at the gate).
    shared.set_review_level(ReviewLevel::HumanOnly);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "human_only");
    assert_eq!(v["autonomous"], false);

    // Autonomous: the derived mirror flips true.
    shared.set_review_level(ReviewLevel::Autonomous);
    assert!(shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "autonomous");
    assert_eq!(v["autonomous"], true);
}

/// `GET /state` surfaces the live background processes (docs/supervisor.md) — the data the
/// TUI process panel polls. A running child appears with its handle/command;
/// `bg_snapshot`/`state_json` read the registry synchronously after spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_json_surfaces_running_processes() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.spawn_background("sleep 5".into()).expect("spawn");
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    let procs = v["processes"]
        .as_array()
        .expect("processes array on /state");
    assert_eq!(procs.len(), 1);
    assert_eq!(procs[0]["handle"], "bg#1");
    assert_eq!(procs[0]["command"], "sleep 5");
    assert!(
        procs[0]["running_secs"].is_u64(),
        "timing present: {procs:?}"
    );
    shared.kill_all_bg();
}
