//! Trailing cache-breakpoint placement (docs/log-and-projection.md).

use super::*;

/// The breakpoint walks back to the last **text** block and marks exactly it —
/// the cached prefix bytes are untouched, trailing tool blocks stay unmarked
/// (docs/log-and-projection.md): a marker converted onto a non-text block bills
/// an unreadable cache write on OpenAI-style explicit caching.
#[test]
fn trailing_cache_breakpoint_marks_only_the_last_text_block() {
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
    // sample_log ends assistant(text, tool_use) → user(tool_result); the marker must
    // land on the text block, leaving the tool_use/tool_result suffix unmarked.
    let marked: Vec<&ApiBlock> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| {
            serde_json::to_string(b).unwrap().contains("cache_control")
        })
        .collect();
    assert!(
        matches!(marked.as_slice(), [ApiBlock::Text { .. }]),
        "the marker sits on a text block: {wire}"
    );
    let after_marker: Vec<&ApiBlock> = messages
        .iter()
        .flat_map(|m| &m.content)
        .skip_while(|b| !serde_json::to_string(b).unwrap().contains("cache_control"))
        .skip(1)
        .collect();
    assert!(
        after_marker
            .iter()
            .all(|b| !matches!(b, ApiBlock::Text { .. })),
        "no text block may follow the marker — it must be the LAST text block"
    );
}

/// Re-placement on an identical wire is byte-identical (the keepalive resends the
/// same prefix).
#[test]
fn trailing_cache_breakpoint_is_idempotent() {
    let mut once = project(&sample_log());
    place_trailing_cache_breakpoint(&mut once);
    let mut twice = once.clone();
    place_trailing_cache_breakpoint(&mut twice);
    assert_eq!(
        serde_json::to_string(&once).unwrap(),
        serde_json::to_string(&twice).unwrap()
    );
}

/// A wire with no text block anywhere gets no marker at all — honest, rather than
/// paying a write that can never be read back (docs/log-and-projection.md).
#[test]
fn no_text_block_means_no_breakpoint() {
    let mut messages = vec![
        ApiMessage {
            role: Role::Assistant,
            content: vec![ApiBlock::ToolUse {
                id: "toolu_1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: Role::User,
            content: vec![ApiBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "file.txt".into(),
                is_error: false,
                cache_control: None,
            }],
        },
    ];
    place_trailing_cache_breakpoint(&mut messages);
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("cache_control")
    );
}

/// Placement on an empty wire is a no-op, not a panic.
#[test]
fn trailing_cache_breakpoint_on_empty_is_a_noop() {
    let mut empty: Vec<ApiMessage> = Vec::new();
    place_trailing_cache_breakpoint(&mut empty);
    assert!(empty.is_empty());
}
