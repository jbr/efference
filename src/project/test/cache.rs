//! Trailing cache-breakpoint placement (docs/log-and-projection.md).

use super::*;

/// The trailing breakpoint marks exactly the last block as `ephemeral` and
/// nothing else — the cached prefix bytes are untouched, only the tail gains
/// the marker (docs/log-and-projection.md).
#[test]
fn trailing_cache_breakpoint_marks_only_the_last_block() {
    let mut messages = project(&sample_log());
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("cache_control"),
        "pure projection carries no breakpoint"
    );

    place_trailing_cache_breakpoint(&mut messages);
    let wire = serde_json::to_string(&messages).unwrap();
    assert_eq!(
        wire.matches("cache_control").count(),
        1,
        "exactly one breakpoint"
    );
    assert!(
        wire.ends_with(r#""cache_control":{"type":"ephemeral"}}]}]"#),
        "breakpoint sits on the final block: {wire}"
    );
}

/// Placement on an empty wire is a no-op, not a panic.
#[test]
fn trailing_cache_breakpoint_on_empty_is_a_noop() {
    let mut empty: Vec<ApiMessage> = Vec::new();
    place_trailing_cache_breakpoint(&mut empty);
    assert!(empty.is_empty());
}
