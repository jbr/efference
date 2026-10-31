//! Message coalescing — how adjacent results and turns merge into wire messages.

use super::*;

/// `is_error: false` must be omitted (lean wire); a true error must appear.
#[test]
fn tool_result_omits_false_is_error_but_keeps_true() {
    let ok = serde_json::to_string(&project(&sample_log())).unwrap();
    assert!(!ok.contains("is_error"));

    let errored = vec![
        entry(
            0,
            0,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new("bash", "x", json!({}))],
            )),
        ),
        entry(
            1,
            0,
            Payload::tool_result("x", RawResult::error("boom"), WireState::Raw),
        ),
    ];
    assert!(
        serde_json::to_string(&project(&errored))
            .unwrap()
            .contains(r#""is_error":true"#)
    );
}

/// Results answering one assistant turn collapse into a single user message.
#[test]
fn consecutive_tool_results_coalesce_into_one_user_message() {
    let log = vec![
        entry(
            0,
            0,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![
                    ToolCall::new("bash", "a", json!({})),
                    ToolCall::new("bash", "b", json!({})),
                ],
            )),
        ),
        entry(
            1,
            0,
            Payload::tool_result("a", RawResult::text("out-a"), WireState::Raw),
        ),
        entry(
            2,
            0,
            Payload::tool_result("b", RawResult::text("out-b"), WireState::Raw),
        ),
    ];
    let messages = project(&log);
    assert_eq!(
        messages.len(),
        2,
        "assistant turn, then ONE coalesced user turn"
    );
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(
        messages[1].content.len(),
        2,
        "both tool_results in one message"
    );
}

/// The projection shape of a queued turn delivered mid-task (docs/log-and-projection.md): a `User`
/// entry appended right after a turn's `tool_result`s projects as **one** user
/// message `[tool_result…, text]`, not two consecutive user-role messages —
/// preserving role-alternation and byte-stability. This is the shape both the
/// mid-trajectory injection and the redirect steer rely on; pinned here so a
/// change to the merge breaks a test before it can silently split the wire.
#[test]
fn user_turn_after_tool_result_merges_into_one_message() {
    let log = vec![
        entry(
            0,
            0,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new("bash", "a", json!({}))],
            )),
        ),
        entry(
            1,
            0,
            Payload::tool_result("a", RawResult::text("tool output"), WireState::Raw),
        ),
        // Injected mid-trajectory (or a redirect steer): a user turn right after
        // the result, with no assistant between.
        entry(2, 1, Payload::user("actually, do X instead")),
    ];
    let messages = project(&log);
    // assistant(tool_use) · ONE user message carrying [tool_result, text].
    assert_eq!(
        messages.len(),
        2,
        "the user turn merges into the tool_result message, not a second user message: {messages:#?}"
    );
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(
        messages[1].content.len(),
        2,
        "tool_result then text, in one user message"
    );
    assert!(
        matches!(&messages[1].content[0], ApiBlock::ToolResult { .. }),
        "tool_result leads (Anthropic requires it first): {:#?}",
        messages[1].content[0]
    );
    assert!(
        matches!(&messages[1].content[1], ApiBlock::Text { text, .. } if text == "actually, do X instead"),
        "the injected text follows: {:#?}",
        messages[1].content[1]
    );
}
