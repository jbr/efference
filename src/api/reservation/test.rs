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

/// A rejected output cap is not a window overflow, and the two take different
/// retries: the window is searched toward by halving, while a refused cap has one
/// right answer the provider usually states.
#[test]
fn a_rejected_output_cap_is_read_from_the_provider_s_own_phrasing() {
    // Anthropic states the ceiling after a comparison.
    let anthropic = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 200000 > 64000, which is the maximum allowed number of output tokens for claude-opus-4-5-20251101"}}"#;
    assert_eq!(
        output_cap_rejection(anthropic),
        Some(OutputCapRejection::Max(64_000))
    );
    // OpenAI states it as a bound.
    let openai = r#"{"error":{"message":"Invalid 'max_output_tokens': integer above maximum value. Expected a value <= 128000, but got 200000 instead.","type":"invalid_request_error"}}"#;
    assert_eq!(
        output_cap_rejection(openai),
        Some(OutputCapRejection::Max(128_000))
    );
    // Others state it as a phrase, with separators written either way.
    assert_eq!(
        output_cap_rejection(r#"{"message":"max_completion_tokens supports at most 32,768"}"#),
        Some(OutputCapRejection::Max(32_768))
    );
    // Recognized without a number, so the caller halves rather than treating a
    // fixable 400 as terminal.
    assert_eq!(
        output_cap_rejection(r#"{"message":"max_tokens is too large for this model"}"#),
        Some(OutputCapRejection::Unstated)
    );
}

/// Window overflow stays the window's, and an unrelated 400 stays unrelated.
/// Every overflow phrasing names the context, which is what separates them.
#[test]
fn a_window_overflow_is_not_read_as_a_rejected_cap() {
    let anthropic = r#"{"error":{"message":"input length and `max_tokens` exceed context limit: 190000 + 20000 > 200000, decrease input length or `max_tokens` and try again"}}"#;
    let openrouter = r#"{"error":{"message":"This endpoint's maximum context length is 1048576 tokens. However, you requested about 1067113 tokens"}}"#;
    assert!(is_window_overflow(anthropic));
    assert_eq!(output_cap_rejection(anthropic), None);
    assert!(is_window_overflow(openrouter));
    assert_eq!(output_cap_rejection(openrouter), None);
    // A prompt too long on its own names no output parameter; no reservation
    // fixes it, and neither matcher claims it.
    let too_long = r#"{"error":{"message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#;
    assert!(!is_window_overflow(too_long));
    assert_eq!(output_cap_rejection(too_long), None);
    // A floor complaint names the parameter but no ceiling.
    assert_eq!(
        output_cap_rejection(r#"{"error":"max_tokens must be at least 1"}"#),
        None
    );
}

/// A stated ceiling is jumped to rather than searched toward — halving from the
/// unknown-cap default to the floor is six round trips, each a full request.
#[test]
fn a_stated_ceiling_converges_in_one_retry() {
    let mut r = Reservation::default();
    r.compute(UNKNOWN_OUTPUT_CAP, None, 0);
    assert_eq!(r.clamp_to(8192), Some(8192));

    // Not a reduction (the clamp is remembered — the next line reads
    // `last_sent` through `clamp_to`), so the rejection was about something
    // else and the caller falls back to halving rather than retrying the same
    // number forever.

    assert_eq!(r.clamp_to(8192), None);
    assert_eq!(r.clamp_to(99_999), None);

    // Never below the floor, where a request fails either way and a tiny cap cuts
    // a turn mid-call for nothing.
    let mut r = Reservation::default();
    r.compute(UNKNOWN_OUTPUT_CAP, None, 0);
    assert_eq!(r.clamp_to(1), Some(RESERVATION_MIN));

    // Nothing sent yet: there is no reservation to clamp.
    assert_eq!(Reservation::default().clamp_to(8192), None);
}

/// A stated ceiling is remembered, not re-learned every turn. `compute` reads the
/// client's configured cap fresh each request, so without this the correction
/// would last one request and the next turn would buy the same 400.
#[test]
fn a_refused_ceiling_is_remembered_for_the_session() {
    let mut r = Reservation::default();
    assert_eq!(r.compute(UNKNOWN_OUTPUT_CAP, None, 0), UNKNOWN_OUTPUT_CAP);
    assert_eq!(r.clamp_to(8192), Some(8192));
    // The next turn asks for the configured cap again and gets the learned one.
    assert_eq!(r.compute(UNKNOWN_OUTPUT_CAP, None, 0), 8192);
    // The window still shrinks it further when the prompt leaves less room; the
    // learned ceiling is an upper bound, not a floor.
    assert!(r.compute(UNKNOWN_OUTPUT_CAP, Some(10_000), 0) < 8192);
}
