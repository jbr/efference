//! Tests for `metrics.rs`.

use super::*;

/// The keepalive's load-bearing invariant (docs/log-and-projection.md): a ping advances the
/// cache-touch clock (it really did refresh the TTL) but must NOT reset the
/// real-turn clock — that clock gates the give-up cap, and a ping resetting its
/// own deadline would pin the cache forever. So after a real turn then a ping,
/// the cap clock has been ticking strictly longer than the touch clock.
#[test]
fn keepalive_advances_touch_clock_not_cap_clock() {
    let log_path = std::env::temp_dir().join("efference-keepalive-test.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let (shared, _turns_rx) = Session::new(Log::create(&log_path).expect("create log"));

    // Nothing to keep warm yet → both clocks absent.
    assert_eq!(shared.keepalive_clocks(), (None, None));

    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 100,
    };
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage {
            id: None,
            model: None,
            usage: usage.clone(),
        },
    ); // real turn → stamps both clocks
    std::thread::sleep(Duration::from_millis(5));
    shared.record_keepalive(RequestUsage {
        id: None,
        model: None,
        usage,
    }); // ping → advances the touch clock only

    let (touch, turn) = shared.keepalive_clocks();
    let (touch, turn) = (touch.unwrap(), turn.unwrap());
    assert!(
        turn > touch,
        "cap clock {turn:?} must outrun the touch clock {touch:?} a ping reset"
    );
}

/// The per-request usage record (docs/log-and-projection.md): every recorded request
/// appends one origin-tagged JSONL line carrying the upstream id and the four token
/// counts — so per-shape spend is a query, not a reconstruction. Absent an attached
/// file (tests, offline verbs) recording is a no-op, which the other tests exercise.
#[test]
fn recorded_requests_land_in_the_usage_record() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-usage-record-test.jsonl");
    let usage_path = dir.join("efference-usage-record-test-usage.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&usage_path);
    let (shared, _turns_rx) = Session::new(Log::create(&log_path).expect("create log"));
    shared.attach_usage_log(&usage_path);

    let usage = Usage {
        input_tokens: 6,
        output_tokens: 10,
        cache_creation_input_tokens: 9407,
        cache_read_input_tokens: 0,
    };
    shared.record_request(
        RequestOrigin::Peek,
        RequestUsage {
            id: Some("gen-abc123".into()),
            model: None,
            usage: usage.clone(),
        },
    );
    shared.record_keepalive(RequestUsage {
        id: None,
        model: None,
        usage,
    });

    let lines: Vec<Value> = std::fs::read_to_string(&usage_path)
        .expect("usage record exists")
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect();
    let [peek, keepalive] = lines.as_slice() else {
        panic!("one line per request, in order: {lines:?}");
    };
    assert_eq!(peek["origin"], "peek");
    assert_eq!(peek["id"], "gen-abc123");
    assert_eq!(peek["input"], 6);
    assert_eq!(peek["output"], 10);
    assert_eq!(peek["cache_write"], 9407);
    assert_eq!(peek["cache_read"], 0);
    assert!(peek["at_ms"].as_u64().is_some_and(|ms| ms > 0));
    assert_eq!(keepalive["origin"], "keepalive");
    assert!(
        keepalive.get("id").is_none(),
        "an absent id is omitted, not null"
    );
}

/// The cold-read classifier (docs/log-and-projection.md): only main-loop requests
/// move the streak, a warm read clears it, and the three explained cold starts —
/// boot, a model change, an idle gap past the TTL — clear it too, because none of
/// them is evidence about the prefix. The side channels are invisible to it: a spot
/// carries its own prefix, so a cold spot means nothing about the trajectory's.
#[test]
fn cold_streak_counts_only_unexplained_main_loop_reads() {
    let mut streak = ColdStreak::default();
    let t = 1_800_000_000_000u128;
    let m = Some("claude-haiku-4-5");

    // Boot: nothing was written yet, so a cold read is expected.
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t, m, 0),
        Some(CacheRead::ColdStart(ColdStart::Boot))
    );
    assert_eq!(streak.streak(), 0);

    // Two unexplained cold turns in a row.
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 1_000, m, 0),
        Some(CacheRead::Cold)
    );
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 2_000, m, 0),
        Some(CacheRead::Cold)
    );
    assert_eq!(streak.streak(), 2);

    // A cold side channel neither counts nor clears.
    assert_eq!(streak.observe(RequestOrigin::Spot, t + 2_500, m, 0), None);
    assert_eq!(streak.streak(), 2);

    // A warm main-loop read clears it — this is what the sidebar warning waits for.
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 3_000, m, 12_000),
        Some(CacheRead::Warm)
    );
    assert_eq!(streak.streak(), 0);

    // A different model does not share a prefix.
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 4_000, Some("claude-opus-4-8"), 0),
        Some(CacheRead::ColdStart(ColdStart::ModelChange))
    );
    assert_eq!(streak.streak(), 0);

    // Past the TTL with nothing touching the prefix: evicted, not moved.
    let idle = t + 4_000 + 400_000;
    assert_eq!(
        streak.observe(RequestOrigin::Turn, idle, Some("claude-opus-4-8"), 0),
        Some(CacheRead::ColdStart(ColdStart::Idle))
    );

    // A keepalive inside the window touches the prefix, so the gap is measured from
    // the ping — the same turn is now unexplained.
    let mut streak = ColdStreak::default();
    streak.observe(RequestOrigin::Turn, t, m, 5_000);
    streak.observe(RequestOrigin::Keepalive, t + 390_000, m, 5_000);
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 400_000, m, 0),
        Some(CacheRead::Cold),
        "a ping refreshed the TTL, so eviction no longer explains the cold read"
    );
}

/// An aborted attempt is a real HTTP round-trip against the main-loop prefix, but
/// its usage is all-zero by construction (docs/transport.md) — a stream that
/// finalized with no content and no usage. That zero says nothing about whether
/// the prefix moved, so a run of aborted attempts must not accumulate as
/// unexplained cold-read evidence the way a run of real cold turns does.
/// Counting them drives the streak up per aborted attempt — two on one turn
/// reach 2, and a longer bad stretch crosses `COLD_STREAK_WARN` for a prefix
/// that never moved.
#[test]
fn aborted_attempts_do_not_count_as_cold_prefix_evidence() {
    let mut streak = ColdStreak::default();
    let t = 1_800_000_000_000u128;
    let m = Some("claude-haiku-4-5");

    // Boot, explained — establishes the touch clock.
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t, m, 0),
        Some(CacheRead::ColdStart(ColdStart::Boot))
    );
    assert_eq!(streak.streak(), 0);

    // A run of aborted attempts, all zero-usage, riding the same prefix.
    assert_eq!(
        streak.observe(RequestOrigin::AbortedAttempt, t + 1_000, m, 0),
        None
    );
    assert_eq!(
        streak.observe(RequestOrigin::AbortedAttempt, t + 2_000, m, 0),
        None
    );
    assert_eq!(
        streak.observe(RequestOrigin::AbortedAttempt, t + 3_000, m, 0),
        None
    );
    assert_eq!(
        streak.streak(),
        0,
        "aborted attempts must not move the streak"
    );

    // The retry that finally finalizes is a genuine unexplained cold read (no
    // eviction, no model change) — recorded as `Turn`, not `AbortedAttempt`.
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 4_000, m, 0),
        Some(CacheRead::Cold)
    );
    assert_eq!(streak.streak(), 1);
}

/// The streak the daemon publishes is the one `GET /state` carries and the sidebar
/// reads (docs/control-api.md), folded as each request is recorded.
#[test]
fn recorded_requests_move_the_published_cold_streak() {
    let log_path = std::env::temp_dir().join("efference-cold-streak-test.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let (shared, _turns_rx) = Session::new(Log::create(&log_path).expect("create log"));
    let turn = |read| RequestUsage {
        id: None,
        model: Some("claude-haiku-4-5".into()),
        usage: Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: read,
        },
    };

    shared.record_request(RequestOrigin::Turn, turn(0)); // boot — explained
    assert_eq!(shared.cold_streak(), 0);
    shared.record_request(RequestOrigin::Turn, turn(0));
    shared.record_request(RequestOrigin::Turn, turn(0));
    assert_eq!(shared.cold_streak(), 2);
    shared.record_request(RequestOrigin::Turn, turn(9_000));
    assert_eq!(shared.cold_streak(), 0, "a warm read clears the warning");
}

/// The token-level measure beside the read rate (docs/log-and-projection.md). A
/// lineage grows monotonically through its turns, and a request that read everything
/// previously sent owes nothing; one that read nothing owes the whole watermark.
#[test]
fn excess_is_the_prefix_a_request_paid_for_twice() {
    let mut excess = ExcessUncached::default();
    let m = Some("gpt-6-astra");

    // Boot: nothing has been sent, so nothing was reusable and nothing is owed.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 6_754, 0),
        Some(Excess {
            expected: 0,
            paid: 0
        })
    );

    // The next turn could have reused the first's whole prompt, floored to a block:
    // 6754 → 6656. Reading exactly that owes nothing.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 7_305, 6_656),
        Some(Excess {
            expected: 6_656,
            paid: 0
        })
    );

    // Reading nothing owes all of it — this is the cold request the rate also catches.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 7_800, 0),
        Some(Excess {
            expected: 7_296,
            paid: 7_296
        })
    );

    // And a preamble-only hit, which the *rate* calls warm, owes the difference.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 9_000, 1_280),
        Some(Excess {
            expected: 7_680,
            paid: 6_400
        }),
        "a warm-by-rate request that matched only the preamble is not warm by tokens"
    );
}

/// A peek's prompt carries a raw result tail that never becomes canonical, so it is
/// measured against the turn watermark but must not raise it — otherwise the turn
/// after it, which is shorter again, reads as having under-matched by the whole tail.
#[test]
fn a_peek_is_measured_against_the_turn_watermark_and_does_not_raise_it() {
    let mut excess = ExcessUncached::default();
    let m = Some("gpt-6-astra");
    excess.observe(RequestOrigin::Turn, m, 6_754, 0);

    // The peek sends the turn's prefix plus 3.5k of raw result; only the prefix was
    // ever cacheable, so that is all it is entitled to.
    assert_eq!(
        excess.observe(RequestOrigin::Peek, m, 10_239, 6_656),
        Some(Excess {
            expected: 6_656,
            paid: 0
        })
    );

    // The turn after it is shorter than the peek and still owes nothing: the peek's
    // tail never entered the watermark.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 7_305, 6_656),
        Some(Excess {
            expected: 6_656,
            paid: 0
        }),
        "the peek's raw tail must not become an expectation on the next turn"
    );
}

/// A turn whose prompt is shorter than the watermark has had its history rewritten
/// under it — a consolidation (docs/consolidate.md). The prefix legitimately moved,
/// so the bounds reset instead of charging every later request for the fold.
#[test]
fn a_shorter_turn_resets_the_watermark() {
    let mut excess = ExcessUncached::default();
    let m = Some("claude-haiku-4-5");
    excess.observe(RequestOrigin::Turn, m, 50_000, 0);
    excess.observe(RequestOrigin::Turn, m, 60_000, 49_920);

    // The fold lands: the transcript is now far shorter than what came before.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 12_000, 0),
        Some(Excess {
            expected: 0,
            paid: 0
        }),
        "a consolidation is the harness's own decision, not a cache miss"
    );

    // And the lineage rebuilds from the new, shorter prefix.
    assert_eq!(
        excess.observe(RequestOrigin::Turn, m, 13_000, 11_904),
        Some(Excess {
            expected: 11_904,
            paid: 0
        })
    );
}

/// Lineages are keyed by class *and* model, which is what separates a fork peek that
/// borrows the agent's client from a `--triage-model` one over the same transcript
/// (docs/redaction-fork.md) — without the report knowing which flags the session
/// booted with. The spotter's requests carry their own conversation either way.
#[test]
fn a_separate_peek_model_is_its_own_lineage() {
    let mut excess = ExcessUncached::default();
    let agent = Some("gpt-6-astra");
    let triage = Some("gpt-5.6-luna");
    excess.observe(RequestOrigin::Turn, agent, 40_000, 0);

    // The triage model has never been sent anything, so its first peek owes nothing
    // despite the agent lineage standing at 40k.
    assert_eq!(
        excess.observe(RequestOrigin::Peek, triage, 44_000, 0),
        Some(Excess {
            expected: 0,
            paid: 0
        }),
        "a second model is a second lineage over the same transcript"
    );

    // A borrowed peek, on the agent's own model, is measured against the agent's.
    assert_eq!(
        excess.observe(RequestOrigin::Peek, agent, 44_000, 0),
        Some(Excess {
            expected: 39_936,
            paid: 39_936
        })
    );

    // The spotter is its own conversation whatever model serves it.
    assert_eq!(
        excess.observe(RequestOrigin::Spot, agent, 6_200, 0),
        Some(Excess {
            expected: 0,
            paid: 0
        })
    );
}

/// An aborted attempt commonly carries all-zero usage (docs/transport.md), so it is
/// no evidence about what the prefix was worth and is not measured.
#[test]
fn an_aborted_attempt_has_no_excess() {
    let mut excess = ExcessUncached::default();
    let m = Some("gpt-6-astra");
    excess.observe(RequestOrigin::Turn, m, 20_000, 0);
    assert_eq!(excess.observe(RequestOrigin::AbortedAttempt, m, 0, 0), None);
}
