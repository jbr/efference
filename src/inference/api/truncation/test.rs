use super::*;

/// The measured Ollama case is caught; an ordinary ratio is not.
#[test]
fn a_short_read_is_named_and_a_full_one_is_not() {
    let why = truncated(2_050, 40_453).expect("Ollama's 4096-token read is truncation");
    assert!(why.contains("2050"), "{why}");
    // 3.5 and 4.4 bytes per token, the corpus's range, read as whole.
    assert_eq!(truncated(11_558, 40_453), None);
    assert_eq!(truncated(9_194, 40_453), None);
}

#[test]
fn small_or_uncounted_requests_are_not_judged() {
    assert_eq!(truncated(10, 4_000), None);
    assert_eq!(truncated(0, 400_000), None);
    // A test double reporting one token for a full harness request.
    assert_eq!(truncated(1, 400_000), None);
}
