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
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );

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
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
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
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
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

/// Only turns and keepalive pings stamp the cache-touch clock: a spotter assessment
/// or a peek may have read some other prefix, and counting it would keep the main
/// cache looking warm after it expired.
#[test]
fn only_main_prefix_requests_stamp_the_touch_clock() {
    let log_path = std::env::temp_dir().join("efference-touch-origin-test.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
    let request = || RequestUsage {
        id: None,
        model: None,
        usage: Usage::default(),
    };
    shared.record_request(RequestOrigin::Spot, request());
    shared.record_request(RequestOrigin::Peek, request());
    let (touch, turn) = shared.keepalive_clocks();
    assert_eq!(touch, None, "side requests leave the touch clock alone");
    assert!(turn.is_some(), "but they are activity for the give-up cap");
    shared.record_request(RequestOrigin::Turn, request());
    assert!(shared.keepalive_clocks().0.is_some());
}

/// A restarted host reads its clocks back from `usage.jsonl`: the touch clock from the
/// last turn or ping, the real-turn clock from the last request that was not a ping.
#[test]
fn attaching_the_usage_record_seeds_the_clocks() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-seed-clocks-test.jsonl");
    let usage_path = dir.join("efference-seed-clocks-test-usage.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let now = crate::log::Timestamp::now().0 as u128;
    let minutes_ago = |m: u128| now - m * 60_000;
    let lines = [
        (minutes_ago(30), "turn"),
        (minutes_ago(20), "keepalive"),
        (minutes_ago(10), "spot"),
        (minutes_ago(9), "not-an-origin-this-build-knows"),
    ];
    let record: String = lines
        .iter()
        .map(|(at, origin)| format!("{{\"at_ms\":{at},\"origin\":\"{origin}\"}}\n"))
        .chain(std::iter::once("not json\n".to_string()))
        .collect();
    std::fs::write(&usage_path, record).unwrap();
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
    shared.attach_usage_log(&usage_path);

    let (touch, turn) = shared.keepalive_clocks();
    let minutes = |d: Option<Duration>| d.unwrap().as_secs() / 60;
    assert_eq!(minutes(touch), 20, "the ping is the last main-prefix touch");
    assert_eq!(
        minutes(turn),
        9,
        "the last non-ping request is the last activity"
    );
}

/// A primer plants a resume point on the triage lineage, not the loop's: it neither
/// counts as prefix evidence nor stamps the touch clock, so the main prefix still
/// reads evicted after an idle gap (backlog/separate-triage-cold-prefix.md).
#[test]
fn primer_neither_counts_nor_touches_the_main_prefix() {
    let mut streak = ColdStreak::default();
    let t = 1_800_000_000_000u128;
    let m = Some("claude-haiku-4-5");

    streak.observe(RequestOrigin::Turn, t, m, 5_000);
    assert_eq!(
        streak.observe(RequestOrigin::TriagePrimer, t + 390_000, m, 5_000),
        None
    );
    assert_eq!(
        streak.observe(RequestOrigin::Turn, t + 400_000, m, 0),
        Some(CacheRead::ColdStart(ColdStart::Idle)),
        "a primer is not a main-prefix touch, so eviction still explains the cold read"
    );
}

/// Primer spend lands in the usage record under its own origin so `dev cache` prices
/// the warmup separately from the peeks it warms.
#[test]
fn primer_requests_land_in_the_usage_record_as_triage_primer() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-usage-primer-test.jsonl");
    let usage_path = dir.join("efference-usage-primer-test-usage.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&usage_path);
    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
    shared.attach_usage_log(&usage_path);

    shared.record_request(
        RequestOrigin::TriagePrimer,
        RequestUsage {
            id: None,
            model: None,
            usage: Usage {
                input_tokens: 140_000,
                output_tokens: 16,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 130_000,
            },
        },
    );

    let lines: Vec<Value> = std::fs::read_to_string(&usage_path)
        .expect("usage record exists")
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect();
    let [primer] = lines.as_slice() else {
        panic!("one line for the primer request: {lines:?}");
    };
    assert_eq!(primer["origin"], "triage_primer");
    assert_eq!(primer["output"], 16);
}
