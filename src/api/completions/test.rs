use super::*;
use crate::log::{CallId, ToolCall};

fn result(call_id: &str, text: &str, images: Vec<ResultImage>) -> UserPart {
    UserPart::ToolResult {
        call_id: CallId(call_id.into()),
        seq: Some(3),
        content: text.into(),
        is_error: false,
        images,
    }
}

fn png() -> ResultImage {
    ResultImage {
        media_type: "image/png".into(),
        data: "AAAA".into(),
        width: 1,
        height: 1,
    }
}

fn completions_turn(message: Value) -> WireTurn {
    let Value::Object(message) = message else {
        panic!("a message is an object");
    };
    WireTurn::Assistant(AssistantOutput::Completions(
        CompletionsMessage::from_assembled(message),
    ))
}

fn messages_turn() -> WireTurn {
    WireTurn::Assistant(AssistantOutput::Messages(vec![
        MessagesOutputBlock::Thinking {
            text: "look first".into(),
            signature: "SIG".into(),
        },
        MessagesOutputBlock::RedactedThinking {
            data: "OPAQUE".into(),
        },
        MessagesOutputBlock::Text("on it".into()),
        MessagesOutputBlock::ToolUse(ToolCall::new(
            "bash",
            "toolu_1",
            serde_json::json!({"description": "list", "command": "ls"}),
        )),
    ]))
}

/// The whole request, byte for byte — the Completions analogue of
/// `request_envelope_matches_expected_wire_bytes`. A reorder or rename here moves
/// the production cache, so it must break this first. The assistant turn is a
/// Mistral-shaped one, thinking parts included, and replays exactly as stored
/// apart from what assembly drops (`null`, `role`, `index`).
#[test]
fn completions_envelope_matches_expected_wire_bytes() {
    let turns = vec![
        WireTurn::text("hello"),
        completions_turn(serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": [{"type": "text", "text": "look"}], "closed": true},
                {"type": "text", "text": "on it"},
            ],
            "tool_calls": [{
                "index": 0,
                "id": "8ufwVNVUd",
                "type": "function",
                "function": {"name": "bash", "arguments": "{\"command\": \"ls\"}"},
            }],
            "refusal": null,
        })),
        WireTurn::User(vec![
            result("8ufwVNVUd", "[3] a.txt", vec![png()]),
            UserPart::Text("and then?".into()),
        ]),
        WireTurn::Breakpoint,
    ];
    let tools = vec![serde_json::json!({
        "name": "bash",
        "description": "run",
        "input_schema": {"type": "object"},
    })];
    let options = RequestOptions::default();
    let request = completions_request(
        "model-x",
        encode_completions(&turns, "PRE"),
        &tools,
        ToolChoice::auto(),
        RequestParams {
            max_tokens: MAX_TOKENS,
            temperature: None,
            effort: None,
            options: &options,
        },
    );
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        concat!(
            r#"{"model":"model-x","max_tokens":4096,"messages":["#,
            r#"{"role":"system","content":"PRE"},"#,
            r#"{"role":"user","content":"hello"},"#,
            r#"{"role":"assistant","content":["#,
            r#"{"closed":true,"thinking":[{"text":"look","type":"text"}],"type":"thinking"},"#,
            r#"{"text":"on it","type":"text"}],"#,
            r#""tool_calls":[{"function":{"arguments":"{\"command\": \"ls\"}","name":"bash"},"id":"8ufwVNVUd","type":"function"}]},"#,
            r#"{"role":"tool","tool_call_id":"8ufwVNVUd","content":"[3] a.txt"},"#,
            r#"{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}},{"type":"text","text":"and then?"}]}],"#,
            r#""tools":[{"function":{"description":"run","name":"bash","parameters":{"type":"object"}},"type":"function"}],"#,
            r#""tool_choice":"auto","stream":true,"stream_options":{"include_usage":true}}"#,
        )
    );
}

/// The connection's settings in the body: the cap under its configured spelling,
/// the session's effort as `reasoning_effort`, and the `body` fields after every
/// encoder-written key, in sorted order.
#[test]
fn request_options_shape_the_envelope() {
    let Value::Object(body) =
        serde_json::json!({"safe_prompt": true, "parallel_tool_calls": false})
    else {
        unreachable!()
    };
    let options = RequestOptions::new(body, Some("max_completion_tokens")).unwrap();
    let request = completions_request(
        "m",
        encode_completions(&[WireTurn::text("hi")], ""),
        &[],
        ToolChoice::auto(),
        RequestParams {
            max_tokens: 100,
            temperature: Some(0.5),
            effort: Some("high"),
            options: &options,
        },
    );
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        concat!(
            r#"{"model":"m","max_completion_tokens":100,"#,
            r#""messages":[{"role":"user","content":"hi"}],"tools":[],"tool_choice":"auto","#,
            r#""temperature":0.5,"reasoning_effort":"high","stream":true,"#,
            r#""stream_options":{"include_usage":true},"#,
            r#""parallel_tool_calls":false,"safe_prompt":true}"#,
        )
    );
}

/// The view's translation of a Messages turn, byte for byte. Nothing sends it, so
/// no cache depends on these bytes; the pin is what notices the translation
/// drifting from the projection it mirrors.
#[test]
fn translated_messages_turn_matches_expected_bytes() {
    let turns = vec![WireTurn::text("hello"), messages_turn()];
    let messages = translate_completions(&turns, "", "claude-haiku-4-5").unwrap();
    assert_eq!(
        serde_json::to_string(&messages).unwrap(),
        concat!(
            r#"[{"role":"user","content":"hello"},"#,
            r#"{"role":"assistant","content":"on it","#,
            r#""reasoning_details":["#,
            r#"{"format":"anthropic-claude-v1","id":null,"index":0,"signature":"SIG","text":"look first","type":"reasoning.text"},"#,
            r#"{"data":"OPAQUE","format":"anthropic-claude-v1","id":null,"index":1,"type":"reasoning.encrypted"}],"#,
            r#""tool_calls":[{"function":{"arguments":"{\"command\":\"ls\",\"description\":\"list\"}","name":"bash"},"id":"toolu_1","type":"function"}]}]"#,
        )
    );
}

/// A run of results with no images and no prose leaves no trailing `user`
/// message.
#[test]
fn results_alone_add_no_user_message() {
    let turns = vec![WireTurn::User(vec![
        result("a", "one", Vec::new()),
        result("b", "two", Vec::new()),
    ])];
    assert_eq!(
        serde_json::to_string(&encode_completions(&turns, "")).unwrap(),
        concat!(
            r#"[{"role":"tool","tool_call_id":"a","content":"one"},"#,
            r#"{"role":"tool","tool_call_id":"b","content":"two"}]"#,
        )
    );
}

/// The live encoder skips another dialect's turns (`check_dialect` keeps them
/// out of a live request); the view translates Messages and refuses Responses.
#[test]
fn foreign_turns_are_skipped_translated_or_refused() {
    let messages = vec![messages_turn()];
    assert!(encode_completions(&messages, "").is_empty());
    assert_eq!(
        translate_completions(&messages, "", "claude-haiku-4-5")
            .unwrap()
            .len(),
        1
    );
    let responses = vec![WireTurn::Assistant(AssistantOutput::Responses(Vec::new()))];
    assert!(translate_completions(&responses, "", "claude-haiku-4-5").is_err());
}

#[test]
fn reasoning_format_follows_the_model_id() {
    assert_eq!(reasoning_format("google/gemini-3-pro"), "google-gemini-v1");
    assert_eq!(reasoning_format("claude-haiku-4-5"), "anthropic-claude-v1");
}
