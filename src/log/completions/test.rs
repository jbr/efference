use super::*;

fn message(value: Value) -> CompletionsMessage {
    let Value::Object(map) = value else {
        panic!("a message is an object");
    };
    CompletionsMessage::from_assembled(map)
}

/// Assembly keeps every key a vendor sent and drops only the stream's own
/// bookkeeping: nulls, `role`, and each call's `index`.
#[test]
fn assembly_drops_only_stream_bookkeeping() {
    let stored = message(serde_json::json!({
        "role": "assistant",
        "content": null,
        "reasoning_content": "think",
        "tool_calls": [{"index": 0, "id": "c", "type": "function",
                        "function": {"name": "bash", "arguments": "{}"},
                        "extra_content": {"google": {"thought_signature": "SIG"}}}],
    }));
    assert_eq!(
        serde_json::to_value(&stored).unwrap(),
        serde_json::json!({
            "reasoning_content": "think",
            "tool_calls": [{"id": "c", "type": "function",
                            "function": {"name": "bash", "arguments": "{}"},
                            "extra_content": {"google": {"thought_signature": "SIG"}}}],
        })
    );
}

/// DeepSeek's `reasoning_content` and OpenRouter's `reasoning` are flat strings;
/// OpenRouter repeats the same text inside `reasoning_details`, which is not read
/// a second time.
#[test]
fn flat_reasoning_is_read_once() {
    let stored = message(serde_json::json!({
        "content": "done",
        "reasoning": "step one",
        "reasoning_details": [{"type": "reasoning.text", "text": "step one",
                               "signature": "SIG", "format": "anthropic-claude-v1"}],
    }));
    assert_eq!(stored.reasoning_text(), vec!["step one"]);
    assert_eq!(stored.text(), vec!["done"]);
}

/// Mistral sends reasoning as `thinking` content parts; the display reads their
/// nested text, and the prose is the `text` part alone.
#[test]
fn thinking_parts_are_reasoning_and_text_parts_are_prose() {
    let stored = message(serde_json::json!({
        "content": [
            {"type": "thinking", "thinking": [{"type": "text", "text": "look"}], "closed": true},
            {"type": "text", "text": "answer"},
        ],
    }));
    assert_eq!(stored.reasoning_text(), vec!["look"]);
    assert_eq!(stored.text(), vec!["answer"]);
}

/// Only structured reasoning: the readable strings come out and the opaque
/// `data` and `signature` stay out.
#[test]
fn structured_reasoning_reads_only_readable_keys() {
    let stored = message(serde_json::json!({
        "content": "",
        "reasoning_details": [
            {"type": "reasoning.text", "text": "plan", "format": "google-gemini-v1"},
            {"type": "reasoning.encrypted", "data": "Ci8B", "format": "google-gemini-v1"},
        ],
    }));
    assert_eq!(stored.reasoning_text(), vec!["plan"]);
    assert!(stored.text().is_empty());
}

/// Calls parse their argument string for execution; an empty string is a call
/// with no arguments, and a malformed one is an error rather than a guess.
#[test]
fn tool_calls_parse_their_arguments() {
    let stored = message(serde_json::json!({
        "tool_calls": [
            {"id": "a", "type": "function", "function": {"name": "bash", "arguments": "{\"command\": \"ls\"}"}},
            {"id": "b", "type": "function", "function": {"name": "noop", "arguments": ""}},
        ],
    }));
    assert_eq!(
        stored.tool_calls().unwrap(),
        vec![
            ToolCall::new("bash", "a", serde_json::json!({"command": "ls"})),
            ToolCall::new("noop", "b", serde_json::json!({})),
        ]
    );
    let broken = message(serde_json::json!({
        "tool_calls": [{"id": "a", "function": {"name": "bash", "arguments": "{\"comm"}}],
    }));
    assert!(broken.tool_calls().is_err());
}

#[test]
fn empty_means_no_output() {
    assert!(message(serde_json::json!({"role": "assistant", "content": ""})).is_empty());
    assert!(CompletionsMessage::default().is_empty());
    assert!(!message(serde_json::json!({"reasoning_content": "x"})).is_empty());
}
