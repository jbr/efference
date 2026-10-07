//! Trailing cache-breakpoint placement (docs/log-and-projection.md).

use super::*;

/// Under `LastTextBlock` the breakpoint walks back to the last **text** block
/// and marks exactly it — the cached prefix bytes are untouched, trailing tool
/// blocks stay unmarked (docs/log-and-projection.md): a marker converted onto a
/// non-text block bills an unreadable cache write on OpenAI-style explicit
/// caching.
#[test]
fn trailing_cache_breakpoint_marks_only_the_last_text_block() {
    let mut messages = project(&sample_log());
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("cache_control"),
        "pure projection carries no breakpoint"
    );

    place_trailing_cache_breakpoint(&mut messages, BreakpointPlacement::LastTextBlock);
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
        .filter(|b| serde_json::to_string(b).unwrap().contains("cache_control"))
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
    place_trailing_cache_breakpoint(&mut once, BreakpointPlacement::LastTextBlock);
    let mut twice = once.clone();
    place_trailing_cache_breakpoint(&mut twice, BreakpointPlacement::LastTextBlock);
    assert_eq!(
        serde_json::to_string(&once).unwrap(),
        serde_json::to_string(&twice).unwrap()
    );
}

/// A wire with no text block anywhere gets no marker at all under
/// `LastTextBlock` — honest, rather than paying a write that can never be read
/// back (docs/log-and-projection.md).
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
    place_trailing_cache_breakpoint(&mut messages, BreakpointPlacement::LastTextBlock);
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("cache_control")
    );
    // The same wire under Anthropic's placement takes the marker on its true
    // tail — a `tool_result` — because `cache_control` is legal on any block
    // there, so nothing is left unmarked behind it.
    place_trailing_cache_breakpoint(&mut messages, BreakpointPlacement::TrailingBlock);
    let marked: Vec<&ApiBlock> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| serde_json::to_string(b).unwrap().contains("cache_control"))
        .collect();
    assert!(
        matches!(marked.as_slice(), [ApiBlock::ToolResult { .. }]),
        "the marker sits on the last block, whatever its type: {marked:?}"
    );
}

/// The two placements differ exactly where the wire ends in a tool loop
/// (docs/log-and-projection.md), and the difference is one block: `sample_log`
/// ends assistant(text, tool_use) → user(tool_result), so Anthropic marks the
/// `tool_result` and the portable placement walks back past both to the text.
/// One breakpoint either way — the count is what must never grow.
#[test]
fn placement_moves_the_marker_without_adding_one() {
    let marked = |placement| {
        let mut messages = project(&sample_log());
        place_trailing_cache_breakpoint(&mut messages, placement);
        let wire = serde_json::to_string(&messages).unwrap();
        assert_eq!(wire.matches("cache_control").count(), 1, "{wire}");
        messages
            .iter()
            .flat_map(|m| m.content.clone())
            .find(|b| serde_json::to_string(b).unwrap().contains("cache_control"))
            .expect("a marked block")
    };
    assert!(matches!(
        marked(BreakpointPlacement::TrailingBlock),
        ApiBlock::ToolResult { .. }
    ));
    assert!(matches!(
        marked(BreakpointPlacement::LastTextBlock),
        ApiBlock::Text { .. }
    ));
}

/// Every provider-conditional shaping choice is fixed at boot from the declared
/// provider and never re-derived mid-session (docs/transport.md) — the property
/// that keeps a session's prefix from moving under it. Anthropic takes its
/// native shapes; OpenRouter takes the portable ones.
#[test]
fn placements_are_derived_from_the_declared_provider() {
    use crate::api::Provider;
    assert_eq!(
        Provider::Anthropic.breakpoint_placement("claude-sonnet-5"),
        BreakpointPlacement::TrailingBlock
    );
    assert_eq!(
        Provider::OpenRouter.breakpoint_placement("stealth/ox-alpha"),
        BreakpointPlacement::LastTextBlock
    );
    // A Gemini slug on OpenRouter sends no marker: a marker there freezes the
    // read at it for the whole tool loop (docs/transport.md).
    assert_eq!(
        Provider::OpenRouter.breakpoint_placement("google/gemini-3.8-flash"),
        BreakpointPlacement::None
    );
    assert_eq!(
        Provider::OpenRouter.breakpoint_placement("google/gemma-3-27b"),
        BreakpointPlacement::LastTextBlock
    );
    // An Anthropic slug on OpenRouter takes the native trailing marker: the
    // text-block marker froze its read across a tool loop.
    assert_eq!(
        Provider::OpenRouter.breakpoint_placement("anthropic/claude-haiku-4.5"),
        BreakpointPlacement::TrailingBlock
    );
    assert_eq!(
        Provider::Anthropic.image_placement("claude-sonnet-5"),
        ImagePlacement::InToolResult
    );
    assert_eq!(
        Provider::OpenRouter.image_placement("stealth/ox-alpha"),
        ImagePlacement::FollowingUserBlock
    );
    // A Gemini slug takes the interposed-turn shape; the same slug on the native
    // API does not, since the rejection is the upstream's, not the skin's.
    assert_eq!(
        Provider::OpenRouter.image_placement("google/gemini-3.7-flash"),
        ImagePlacement::AfterInterposedTurn
    );
    assert_eq!(
        Provider::Anthropic.image_placement("google/gemini-3.7-flash"),
        ImagePlacement::InToolResult
    );
    // The author prefix alone does not sweep a non-Gemini slug in.
    assert_eq!(
        Provider::OpenRouter.image_placement("google/gemma-3-27b"),
        ImagePlacement::FollowingUserBlock
    );
}

/// `None` leaves every block unmarked, including a breakpoint a user turn
/// carries inside itself — any `cache_control` would become a Gemini explicit
/// cache and freeze the read there (docs/log-and-projection.md).
#[test]
fn none_placement_marks_nothing() {
    let mut messages = project(&sample_log());
    place_trailing_cache_breakpoint(&mut messages, BreakpointPlacement::None);
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("cache_control")
    );
    let turns = [
        WireTurn::User(vec![UserPart::Text("hello".into()), UserPart::Breakpoint]),
        WireTurn::Breakpoint,
    ];
    let encoded = super::super::messages::encode_messages(
        &turns,
        ImagePlacement::FollowingUserBlock,
        BreakpointPlacement::None,
    );
    assert!(
        !serde_json::to_string(&encoded)
            .unwrap()
            .contains("cache_control")
    );
}

/// Placement on an empty wire is a no-op, not a panic.
#[test]
fn trailing_cache_breakpoint_on_empty_is_a_noop() {
    let mut empty: Vec<ApiMessage> = Vec::new();
    place_trailing_cache_breakpoint(&mut empty, BreakpointPlacement::TrailingBlock);
    place_trailing_cache_breakpoint(&mut empty, BreakpointPlacement::LastTextBlock);
    assert!(empty.is_empty());
}
