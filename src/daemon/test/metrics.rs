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
    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));

    // Nothing to keep warm yet → both clocks absent.
    assert_eq!(shared.keepalive_clocks(), (None, None));

    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 100,
    };
    shared.record_request(RequestOrigin::Turn, RequestUsage { id: None, usage: usage.clone() }); // real turn → stamps both clocks
    std::thread::sleep(Duration::from_millis(5));
    shared.record_keepalive(RequestUsage { id: None, usage }); // ping → advances the touch clock only

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
    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
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
            usage: usage.clone(),
        },
    );
    shared.record_keepalive(RequestUsage { id: None, usage });

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
