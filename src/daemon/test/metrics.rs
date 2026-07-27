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
    shared.record_request(usage.clone()); // real turn → stamps both clocks
    std::thread::sleep(Duration::from_millis(5));
    shared.record_keepalive(usage); // ping → advances the touch clock only

    let (touch, turn) = shared.keepalive_clocks();
    let (touch, turn) = (touch.unwrap(), turn.unwrap());
    assert!(
        turn > touch,
        "cap clock {turn:?} must outrun the touch clock {touch:?} a ping reset"
    );
}
