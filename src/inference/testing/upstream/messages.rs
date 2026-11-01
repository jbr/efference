//! The Anthropic Messages event streams a script answers with (`upstream.rs`).

use serde_json::Value;

/// `data`, each one Messages event's JSON, as an event stream, each event named by its
/// own `type`.
pub(crate) fn events(data: &[String]) -> String {
    data.iter()
        .map(|d| {
            let kind: Value = serde_json::from_str(d).unwrap();
            format!("event: {}\ndata: {d}\n\n", kind["type"].as_str().unwrap())
        })
        .collect()
}

/// A `tool_use` completion calling `name` with `input` (JSON text).
pub(crate) fn tool_call(id: &str, name: &str, input: &str) -> String {
    let input = serde_json::to_string(input).unwrap();
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        format!(
            r#"{{"type":"content_block_start","index":0,"content_block":{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}}}"#
        ),
        format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{input}}}}}"#
        ),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

/// A plain end of turn, whose text is `ok`.
pub(crate) fn end_turn() -> String {
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

/// Whether `body` is a title ask, recognizable by the one phrase no other request
/// carries (docs/session-title.md): the peek's tail instruction.
pub(super) fn is_title_ask(body: &str) -> bool {
    body.contains("say what this session is for")
}
