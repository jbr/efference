//! Tests for `views.rs`.

use super::*;

/// `GET /state` exposes the optional last upstream failure without putting a
/// transport diagnostic into the log (docs/transport.md).
#[test]
fn state_reports_upstream_failure() {
    let failure = crate::api::upstream_failure_state();
    let (shared, _turns_rx) = Session::with_role_scratch_and_upstream_failure(
        std::env::current_dir().unwrap(),
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

/// `GET /state` carries the retry in flight as its own topic — surface, attempt,
/// budget, and when the next attempt fires — and reports `null` once the attempt
/// that finalizes clears it (docs/transport.md).
#[test]
fn state_reports_the_retry_in_flight() {
    let retry = crate::api::upstream_retry_state();
    let (shared, _turns_rx) = Session::full(crate::daemon::SessionParts {
        upstream_retry: Arc::clone(&retry),
        ..crate::daemon::SessionParts::new(std::env::current_dir().unwrap(), Log::in_memory())
    });

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["upstream_retry"].is_null());

    retry.set(crate::api::UpstreamRetry {
        surface: crate::api::RetrySurface::Peek,
        attempt: 2,
        budget: 12,
        next_attempt_at: 1_754_300_000_000,
        message: "stream aborted, retrying (2 of 12)".into(),
        detail: "upstream SSE stream failed mid-stream".into(),
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["upstream_retry"],
        serde_json::json!({
            "surface": "peek",
            "attempt": 2,
            "budget": 12,
            "next_attempt_at": 1_754_300_000_000u64,
            "message": "stream aborted, retrying (2 of 12)",
            "detail": "upstream SSE stream failed mid-stream",
        })
    );

    retry.clear(crate::api::RetrySurface::Peek);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(
        v["upstream_retry"].is_null(),
        "the all-clear is the topic's cleared value, not a residual chip"
    );
}

/// `GET /state` distinguishes the two idle conditions `running: false` collapses:
/// a trajectory that finished, and one that gave up (docs/transport.md). The
/// `stall` topic is set when the loop stops short and cleared when a trajectory
/// starts again, so a supervising client polling an idle daemon can tell them
/// apart.
#[test]
fn state_reports_a_stalled_trajectory() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(
        v["stall"].is_null(),
        "an idle daemon that never gave up reports no stall"
    );

    shared.set_stall(Stall {
        reason: StallReason::RetriesExhausted,
        attempts: Some(12),
        at: 1_754_300_000_000,
        message: "the trajectory stopped: 12 attempts in a row aborted upstream — \
                  send a turn to retry"
            .into(),
        detail: "upstream SSE stream failed mid-stream".into(),
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["stall"],
        serde_json::json!({
            "reason": "retries_exhausted",
            "attempts": 12,
            "at": 1_754_300_000_000u64,
            "message": "the trajectory stopped: 12 attempts in a row aborted upstream — \
                        send a turn to retry",
            "detail": "upstream SSE stream failed mid-stream",
        })
    );

    shared.clear_stall();
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(
        v["stall"].is_null(),
        "a starting trajectory clears the last one's give-up"
    );
}

/// `GET /state` names what the session is — the configured model and its working
/// directory (docs/control-api.md) — as a topic, so a client reads it off the
/// connect snapshot. `null` before boot sets it, which is what a test or an
/// offline verb sees.
#[test]
fn state_reports_the_session_identity() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["session_identity"].is_null());

    shared.set_identity(SessionIdentity {
        id: "amber-heron".into(),
        title: None,
        model: "anthropic/claude-opus-4".into(),
        cwd: "/Users/jbr/code/efference".into(),
        inference: None,
        successor: None,
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["session_identity"],
        serde_json::json!({
            "id": "amber-heron",
            "model": "anthropic/claude-opus-4",
            "cwd": "/Users/jbr/code/efference",
        })
    );

    // A title decision (docs/session-title.md) lands in the log and on the topic.
    let seq = shared
        .set_title(crate::log::Title {
            text: "Session names: a random slug and a triage-kept title".into(),
            author: crate::log::TitleAuthor::Operator,
        })
        .unwrap()
        .expect("an identity was set");
    let entries = shared.snapshot();
    assert!(matches!(
        &entries[seq as usize].payload,
        Payload::Title(t) if t.author == crate::log::TitleAuthor::Operator
    ));
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["session_identity"]["title"],
        "Session names: a random slug and a triage-kept title"
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
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );

    // No request yet: no `last`, no stamp.
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["usage"]["last"].is_null());
    assert!(v["usage"]["last_request_at"].is_null());

    // Cold prefix: all creation, no read.
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage {
            id: None,
            model: None,
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
            model: None,
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
    // An absolute wall-clock stamp now exists — the eviction-prediction signal
    // (docs/log-and-projection.md). Absolute rather than elapsed so a level-triggered
    // `usage` topic does not re-emit on every read (docs/control-api.md); a client
    // subtracts from its own clock.
    let at = v["usage"]["last_request_at"].as_u64().expect("stamped");
    let now = crate::log::Timestamp::now().0;
    assert!(at <= now && now - at < 60_000, "a plausible recent stamp");
    // The cold-read streak rides the same topic (docs/log-and-projection.md): the
    // first turn's cold read was the session's boot, the second was warm, so nothing
    // is outstanding.
    assert_eq!(v["usage"]["cold_streak"], 0);
}

/// `GET /state` reports context-window occupancy (docs/transport.md): null with
/// no boot-fetched capabilities (tests, offline verbs), `{limit, used: null}`
/// once set but before any request, and `used` = the last request's total
/// prompt tokens — uncached input + cache reads + cache writes, not
/// `input_tokens` alone (that is only the uncached remainder).
#[test]
fn state_reports_context_occupancy_against_the_boot_limit() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    // No capabilities fetched → the whole readout is null, not a guessed limit.
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["usage"]["context"].is_null());

    shared.set_capabilities(crate::api::ModelCapabilities {
        context_length: Some(200_000),
        max_output_tokens: Some(64_000),
        supports_image_input: true,
        prompt_cache: crate::api::PromptCachePolicy::Explicit,
        ..crate::api::ModelCapabilities::unknown()
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["context"]["limit"], 200_000);
    assert!(v["usage"]["context"]["used"].is_null());

    // A hot cached request: 6 uncached + 165 written + 9407 read = 9578 prompt
    // tokens actually occupying the window.
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage {
            id: None,
            model: None,
            usage: Usage {
                input_tokens: 6,
                output_tokens: 12,
                cache_creation_input_tokens: 165,
                cache_read_input_tokens: 9407,
            },
        },
    );
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["context"]["limit"], 200_000);
    assert_eq!(v["usage"]["context"]["used"], 6 + 165 + 9407);

    // The ceiling is the window itself and does not move as the prompt grows:
    // near capacity the output reservation shrinks toward its floor
    // (`api/reservation.rs`), so a limit computed from that reservation would
    // rise with the prompt and read as a ceiling the session never hits.
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage {
            id: None,
            model: None,
            usage: Usage {
                input_tokens: 196_000,
                output_tokens: 12,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        },
    );
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["context"]["limit"], 200_000);
    assert_eq!(v["usage"]["context"]["used"], 196_000);
}

/// **A side-channel request must not move reported occupancy.** The fork peek,
/// the idle triage, and the spotter each carry their own much smaller prefix, so
/// folding them into the context readout makes it *drop* mid-turn and correct
/// itself afterwards. Their tokens still belong in the cumulative totals, which
/// is what this pins on both sides.
#[test]
fn context_occupancy_ignores_side_channel_requests() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_capabilities(crate::api::ModelCapabilities {
        context_length: Some(200_000),
        max_output_tokens: Some(64_000),
        supports_image_input: true,
        prompt_cache: crate::api::PromptCachePolicy::Explicit,
        ..crate::api::ModelCapabilities::unknown()
    });
    let usage = |input, read| RequestUsage {
        id: None,
        model: None,
        usage: Usage {
            input_tokens: input,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: read,
        },
    };

    shared.record_request(RequestOrigin::Turn, usage(10, 27_000));
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["context"]["used"], 27_010);

    // Every non-turn origin, each with a prefix a fraction of the trajectory's.
    for origin in [
        RequestOrigin::Peek,
        RequestOrigin::Triage,
        RequestOrigin::Spot,
    ] {
        shared.record_request(origin, usage(8, 6_800));
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        assert_eq!(
            v["usage"]["context"]["used"], 27_010,
            "{origin:?} moved reported occupancy"
        );
    }
    shared.record_keepalive(usage(4, 100));
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["context"]["used"], 27_010, "keepalive moved it");

    // The next real turn does move it — the readout is scoped, not frozen.
    shared.record_request(RequestOrigin::Turn, usage(12, 31_000));
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["context"]["used"], 31_012);

    // Side-channel spend is still counted; scoping `context` did not hide it.
    let cumulative = &v["usage"]["cumulative"];
    assert_eq!(cumulative["input_tokens"], 10 + 8 * 3 + 4 + 12);
    assert_eq!(
        cumulative["cache_read_input_tokens"],
        27_000 + 6_800 * 3 + 100 + 31_000
    );
    // And `last` still means the last request of any kind.
    assert_eq!(v["usage"]["last"]["input_tokens"], 12);
}

/// `GET /state` reports session cost only when a provider accounting surface
/// armed it (docs/transport.md): null unarmed (direct Anthropic, tests), and
/// `{usd, unpriced}` once armed — `unpriced > 0` marks the total an undercount.
#[test]
fn state_reports_session_cost_only_when_armed() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["usage"]["cost"].is_null());

    shared.enable_cost_tracking(crate::api::Endpoint {
        provider: crate::api::Provider::OpenRouter,
        url: Some("https://openrouter.ai/api/v1/messages".into()),
        auth_header: "authorization".into(),
        auth_value: "Bearer sk-or-test".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["usage"]["cost"],
        serde_json::json!({ "usd": 0.0, "unpriced": 0 })
    );

    // Fold what the spawned per-generation fetches would write (the fetch
    // itself is network; the fold and readout are what this pins).
    {
        let mut cell = shared.cost.lock().unwrap();
        cell.total_usd += 0.0421;
        cell.unpriced += 1;
    }
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["cost"]["usd"], 0.0421);
    assert_eq!(v["usage"]["cost"]["unpriced"], 1);
}

/// `GET /state` reports the resolved role (docs/permissions.md): a
/// `null` source for the built-in default, and the path plus the **effective** config's full
/// contents (scope, watch-for, commands, tools, paths) when a config is in effect — so "what is
/// my live perimeter?" is answerable, not just "which file?". The contents track the *effective*
/// policy, so a folded config edit shows up (docs/permissions.md).
#[test]
fn state_reports_effective_role_contents() {
    // Default role (no config file) → null source, empty everywhere.
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["role"]["source"].is_null());
    assert!(v["role"]["scope"].is_null());
    assert_eq!(v["role"]["commands"], serde_json::json!({}));

    // A loaded config surfaces its resolved path and full contents (sorted, by BTree order).
    let loaded = LoadedConfig {
        role: crate::config::Config::from_toml(
            r#"
            scope = "stay in the repo"
            watch-for = ["network egress"]
            tools = ["read"]

            [commands]
            cargo = true
            "git diff" = true
            "git push" = "deny"

            [paths]
            "." = true
            "#,
        )
        .expect("valid config"),
        source: ConfigSource::Path("efference.toml".into()),
    };
    let (shared, _rx) =
        Session::with_role(std::env::current_dir().unwrap(), Log::in_memory(), loaded);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["role"]["source"],
        serde_json::json!(shared.cwd().join("efference.toml"))
    );
    assert_eq!(v["role"]["scope"], "stay in the repo");
    assert_eq!(
        v["role"]["watch_for"],
        serde_json::json!(["network egress"])
    );
    // key → stance, so a carve-out is legible as one rather than reading as a grant.
    assert_eq!(
        v["role"]["commands"],
        serde_json::json!({ "cargo": "clear", "git diff": "clear", "git push": "deny" })
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
        serde_json::json!({
            "cargo": "clear",
            "git diff": "clear",
            "git push": "deny",
            "npm": "clear",
        })
    );
}

/// Autonomous mode defaults off, and `set_autonomous` moves the *dial* — the
/// orchestrator-above reads `review_level` to know whether a subordinate runs unsupervised.
/// `/state` does not restate it as a separate `autonomous` bool: a state topic carries one
/// value, not a value plus a partial mirror of it (docs/control-api.md).
#[test]
fn state_reports_autonomous_mode() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert!(!shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "spotter_human");

    shared.set_autonomous(true);
    assert!(shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "autonomous");
    assert!(v.get("autonomous").is_none(), "not restated as a bool");
}

/// The review-level dial (docs/permissions.md) is runtime-settable and reported on `/state` as
/// the single representation of who reviews a gated call: default `spotter_human`.
#[test]
fn state_reports_the_review_level_dial() {
    use crate::policy::ReviewLevel;
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    // Default: supervised, not autonomous.
    assert_eq!(shared.review_level(), ReviewLevel::SpotterHuman);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "spotter_human");

    // HumanOnly: reported, and still not autonomous (a human is at the gate).
    shared.set_review_level(ReviewLevel::HumanOnly);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "human_only");

    // Autonomous.
    shared.set_review_level(ReviewLevel::Autonomous);
    assert!(shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "autonomous");
}

/// `GET /state` surfaces the live background processes (docs/supervisor.md) — the data the
/// TUI process panel reads. A running child appears with its id/command, its OS pid,
/// and an absolute spawn stamp; `process_states`/`state_json` read the registry
/// synchronously after spawn.
///
/// The topic carries **structure only** (docs/control-api.md): no `tail` and no
/// last-chunk stamp, because both change with every output chunk and would make a
/// level-triggered topic re-emit a whole-topic replacement per chunk. That content rides
/// `process_output` on the ephemeral `output` class. This asserts their absence, since
/// their return would be a silent regression to a per-chunk firehose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_json_surfaces_running_processes() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .spawn_background("sleep 5".into(), None)
        .expect("spawn");
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    let procs = v["processes"]
        .as_array()
        .expect("processes array on /state");
    assert_eq!(procs.len(), 1);
    assert_eq!(
        procs[0]["id"], 1,
        "the bare spawn-minted ordinal, not a handle"
    );
    assert_eq!(procs[0]["command"], "sleep 5");
    assert!(
        procs[0]["pid"].as_u64().is_some(),
        "the OS pid rides as an attribute"
    );
    assert_eq!(
        procs[0]["background"], true,
        "background is a property on the representation, not a namespace"
    );
    assert_eq!(procs[0]["muted"], false, "muted is flagged, not filtered");
    let at = procs[0]["spawned_at"]
        .as_u64()
        .expect("absolute spawn stamp");
    let now = crate::log::Timestamp::now().0;
    assert!(at <= now && now - at < 60_000, "a plausible recent stamp");
    assert!(
        procs[0]["tail"].is_null(),
        "no tail: it rides process_output"
    );
    assert!(
        procs[0]["running_secs"].is_null() && procs[0]["idle_secs"].is_null(),
        "no ticking counters: clients subtract from the absolute stamp"
    );
    shared.kill_all_bg();
}

/// `GET /state` reports cumulative triage retention (docs/redaction-fork.md), folded
/// from the log behind a cursor. Drives the two things the unit tests can't reach: that
/// the cursor *advances* across successive polls rather than refolding or freezing, and
/// that a withheld result stays out of the ratio it would otherwise flatter.
#[test]
fn state_reports_triage_retention() {
    use crate::log::{Distilled, Keep, RawResult, WireState};

    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let result = |seq: u64, lines: usize, wire: WireState| {
        let content = (1..=lines)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        shared
            .append(
                0,
                Payload::tool_result(format!("call_{seq}"), RawResult::text(content), wire),
            )
            .expect("append");
    };

    // Nothing triaged yet: the counters are zero, so a client renders the ratio as
    // absent rather than as 0%.
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["retention"]["shown_lines"], 0);
    assert_eq!(v["usage"]["retention"]["kept_lines"], 0);

    result(
        0,
        100,
        WireState::from(Distilled::new(Keep::Ranges(vec![(1, 40)]))),
    );
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["retention"]["kept_lines"], 40);
    assert_eq!(v["usage"]["retention"]["shown_lines"], 100);
    assert_eq!(v["usage"]["retention"]["withheld"], 0);

    // A second poll after a second result: the fold advanced by exactly the new entry.
    // A cursor that failed to advance would report 40/100 again; one that reset would
    // refold the first result and report 80/200.
    result(1, 100, WireState::from(Distilled::new(Keep::All)));
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["retention"]["kept_lines"], 140);
    assert_eq!(v["usage"]["retention"]["shown_lines"], 200);

    // A failed gate withheld 900 lines. Counted, but on neither side of the ratio —
    // folding it in would read as compression rather than as failure.
    result(
        2,
        900,
        WireState::TriageFailed {
            why: "retries exhausted".into(),
        },
    );
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["usage"]["retention"]["withheld"], 1);
    assert_eq!(v["usage"]["retention"]["kept_lines"], 140);
    assert_eq!(v["usage"]["retention"]["shown_lines"], 200);
}

/// `GET /state` is exactly `last_seq` plus one field per [`Topic`], each named by
/// [`Topic::as_str`] — the structural half of "`GET /state` ≡ the fold of every `state`
/// frame since connect" (docs/control-api.md).
///
/// The fold identity needs the two sides to agree on *names*, not just on values, so this
/// pins the key set from both directions: no topic missing from the snapshot, and no
/// snapshot key that is not a topic. `last_seq` is the one deliberate exception — the log's
/// cursor rather than control state, so it is outside the fold and named here explicitly
/// rather than tolerated by a loose assertion.
#[test]
fn state_is_last_seq_plus_exactly_the_topics() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    let keys: std::collections::BTreeSet<&str> =
        v.as_object().unwrap().keys().map(String::as_str).collect();

    let mut expected: std::collections::BTreeSet<&str> =
        Topic::ALL.iter().map(|t| t.as_str()).collect();
    expected.insert("last_seq");

    assert_eq!(keys, expected, "snapshot keys are last_seq + every topic");

    // And each topic's value is reachable under its own name — i.e. `topic_json` is what
    // the composite is built from, not a parallel implementation that happens to agree.
    for topic in Topic::ALL {
        assert_eq!(
            v[topic.as_str()],
            shared.topic_json(topic),
            "{} matches its topic function",
            topic.as_str()
        );
    }
}

/// `usage.plan` publishes the Codex subscription windows off the shared cell, and
/// is `null` until a Codex response has named them (docs/transport.md).
#[test]
fn state_reports_plan_windows_under_usage() {
    let plan = crate::api::plan_state();
    let (shared, _turns_rx) = Session::full(crate::daemon::SessionParts {
        plan: Arc::clone(&plan),
        ..crate::daemon::SessionParts::new(std::env::current_dir().unwrap(), Log::in_memory())
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["usage"]["plan"].is_null());

    *plan.lock().unwrap() = Some(crate::api::PlanSnapshot {
        plan_type: Some("plus".into()),
        active: None,
        limits: std::collections::BTreeMap::from([(
            "codex".into(),
            crate::api::PlanLimit {
                name: None,
                primary: Some(crate::api::PlanWindow {
                    used_percent: 12.5,
                    window_minutes: Some(300),
                    resets_at: Some(1_788_677_725_000),
                }),
                secondary: None,
            },
        )]),
        observed_at: 5,
    });
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["usage"]["plan"],
        serde_json::json!({
            "type": "plus",
            "active": null,
            "limits": {
                "codex": {
                    "name": null,
                    "primary": { "used_percent": 12.5, "window_minutes": 300, "resets_at": 1_788_677_725_000u64 },
                    "secondary": null
                }
            },
            "observed_at": 5,
        })
    );
}

/// The `cold` topic (backlog: cold-session.md): `null` on a model with no cache and
/// while warm; once the last turn is older than the cache lifetime, the instant the
/// belief began and the context a cold read would pay for. A restarted host reads both
/// from `usage.jsonl`.
#[test]
fn cold_topic_reports_an_expired_cache() {
    let usage_path = std::env::temp_dir().join("efference-cold-topic-test-usage.jsonl");
    let at = crate::log::Timestamp::now().0 - 10 * 60_000;
    std::fs::write(
        &usage_path,
        format!(
            "{{\"at_ms\":{at},\"origin\":\"turn\",\"input\":40,\"cache_read\":9000,\"cache_write\":960}}\n"
        ),
    )
    .unwrap();
    let cold = |ttl: Option<Duration>| {
        let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
        shared.attach_usage_log(&usage_path);
        append_foldable_history(&shared);
        if let Some(ttl) = ttl {
            shared.set_cache_ttl(ttl);
        }
        let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
        v["cold"].clone()
    };
    assert!(cold(None).is_null(), "no cache, no cold state");
    assert!(
        cold(Some(Duration::from_secs(20 * 60))).is_null(),
        "still warm"
    );
    assert_eq!(
        cold(Some(Duration::from_secs(5 * 60))),
        serde_json::json!({ "since": at + 5 * 60_000, "context_tokens": 10_000 })
    );
}
