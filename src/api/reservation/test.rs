use super::*;

/// No window (no capability lookup): the cap goes out untouched.
#[test]
fn without_a_window_the_cap_is_sent() {
    let mut r = Reservation::default();
    assert_eq!(r.compute(64_000, None, 10_000_000), 64_000);
}

/// A prompt far from the wall: the cap is sent, byte-identical to before the
/// reservation existed.
#[test]
fn far_from_the_wall_the_cap_is_sent() {
    let mut r = Reservation::default();
    // 40k bytes → ~13k tokens estimated against a 200k window, 64k cap.
    assert_eq!(r.compute(64_000, Some(200_000), 40_000), 64_000);
}

/// The failure the reservation exists for: a 1M window advertising a 943k cap walls at ~105k of
/// prompt. The reservation shrinks so prompt plus reservation stays inside.
#[test]
fn near_the_wall_the_reservation_shrinks_to_fit() {
    let mut r = Reservation::default();
    r.calibrate(110_000, 440_000);
    let sent = r.compute(943_718, Some(1_048_576), 440_000);
    assert!(sent < 943_718, "the cap alone would overflow: {sent}");
    assert!(110_000 + sent + MARGIN <= 1_048_576, "must fit: {sent}");
    // Everything the window spares is reserved — no more, no less.
    assert_eq!(sent, 1_048_576 - 110_000 - MARGIN);
}

/// Calibration anchors the estimate to the provider's own count; only the
/// appended tail is estimated, at the conservative ratio.
#[test]
fn the_tail_is_estimated_from_the_calibrated_count() {
    let mut r = Reservation::default();
    r.calibrate(150_000, 600_000);
    // 3,000 bytes appended → 1,000 tokens at the tail ratio.
    assert_eq!(r.estimate_prompt(603_000), 151_000);
    // A smaller body (the turn after a peek) estimates downward the same way.
    assert_eq!(r.estimate_prompt(597_000), 149_000);
}

/// A window with nothing to spare still sends the floor rather than zero.
#[test]
fn a_full_window_sends_the_floor() {
    let mut r = Reservation::default();
    r.calibrate(199_000, 800_000);
    assert_eq!(r.compute(64_000, Some(200_000), 800_000), RESERVATION_MIN);
}

/// The overflow retry halves what was last sent, down to the floor, then
/// gives up.
#[test]
fn halving_stops_at_the_floor() {
    let mut r = Reservation::default();
    assert_eq!(r.halve(), None, "nothing sent yet");
    r.compute(4096, None, 0);
    assert_eq!(r.halve(), Some(2048));
    assert_eq!(r.halve(), Some(RESERVATION_MIN));
    assert_eq!(r.halve(), None);
}

/// The two provider phrasings match; a prompt that is simply too long does not.
#[test]
fn window_overflow_is_recognized_by_phrasing() {
    let openrouter = r#"{"type":"error","error":{"type":"invalid_request_error","message":"This endpoint's maximum context length is 1048576 tokens. However, you requested about 1067113 tokens (110149 of text input, 13246 of tool input, 943718 in the output). Please reduce the length of either one, or use the context-compression plugin to compress your prompt automatically."}}"#;
    let anthropic = r#"{"type":"error","error":{"type":"invalid_request_error","message":"input length and `max_tokens` exceed context limit: 190000 + 20000 > 200000, decrease input length or `max_tokens` and try again"}}"#;
    let too_long = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#;
    assert!(is_window_overflow(openrouter));
    assert!(is_window_overflow(anthropic));
    assert!(!is_window_overflow(too_long));
    assert!(!is_window_overflow(
        "{\"error\":\"max_tokens must be at least 1\"}"
    ));
}
