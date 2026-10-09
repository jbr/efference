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

/// The whole view envelope, byte for byte. Nothing sends it yet, so no cache
/// depends on these bytes; the pin is what notices the translation drifting from
/// the projection it mirrors (backlog/completions-dialect/wire-view.md).
#[test]
fn completions_view_matches_expected_wire_bytes() {
    let turns = vec![
        WireTurn::text("hello"),
        messages_turn(),
        WireTurn::User(vec![
            result("toolu_1", "[3] a.txt", vec![png()]),
            UserPart::Text("and then?".into()),
        ]),
        WireTurn::Breakpoint,
    ];
    let tools = vec![serde_json::json!({
        "name": "bash",
        "description": "run",
        "input_schema": {"type": "object"},
    })];
    let request =
        completions_request("model-x", "PRE", &tools, ToolChoice::auto(), &turns).unwrap();
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        concat!(
            r#"{"model":"model-x","max_tokens":4096,"messages":["#,
            r#"{"role":"system","content":"PRE"},"#,
            r#"{"role":"user","content":"hello"},"#,
            r#"{"role":"assistant","content":"on it","#,
            r#""tool_calls":[{"id":"toolu_1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\",\"description\":\"list\"}"}}],"#,
            r#""reasoning_details":["#,
            r#"{"format":"anthropic-claude-v1","id":null,"index":0,"signature":"SIG","text":"look first","type":"reasoning.text"},"#,
            r#"{"data":"OPAQUE","format":"anthropic-claude-v1","id":null,"index":1,"type":"reasoning.encrypted"}]},"#,
            r#"{"role":"tool","tool_call_id":"toolu_1","content":"[3] a.txt"},"#,
            r#"{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}},{"type":"text","text":"and then?"}]}],"#,
            r#""tools":[{"function":{"description":"run","name":"bash","parameters":{"type":"object"}},"type":"function"}],"#,
            r#""tool_choice":"auto","stream":true,"stream_options":{"include_usage":true}}"#,
        )
    );
}

/// A run of results with no images and no prose leaves no trailing `user`
/// message, and an empty signature is sent as `null` rather than `""`.
#[test]
fn results_alone_add_no_user_message() {
    let turns = vec![
        WireTurn::Assistant(AssistantOutput::Messages(vec![
            MessagesOutputBlock::Thinking {
                text: "t".into(),
                signature: String::new(),
            },
            MessagesOutputBlock::ToolUse(ToolCall::new("bash", "a", serde_json::json!({}))),
            MessagesOutputBlock::ToolUse(ToolCall::new("bash", "b", serde_json::json!({}))),
        ])),
        WireTurn::User(vec![
            result("a", "one", Vec::new()),
            result("b", "two", Vec::new()),
        ]),
    ];
    let messages = translate_completions(&turns, "", "google/gemini-3-pro").unwrap();
    assert_eq!(
        serde_json::to_string(&messages).unwrap(),
        concat!(
            r#"[{"role":"assistant","content":"","tool_calls":["#,
            r#"{"id":"a","type":"function","function":{"name":"bash","arguments":"{}"}},"#,
            r#"{"id":"b","type":"function","function":{"name":"bash","arguments":"{}"}}],"#,
            r#""reasoning_details":[{"format":"google-gemini-v1","id":null,"index":0,"signature":null,"text":"t","type":"reasoning.text"}]},"#,
            r#"{"role":"tool","tool_call_id":"a","content":"one"},"#,
            r#"{"role":"tool","tool_call_id":"b","content":"two"}]"#,
        )
    );
}

#[test]
fn a_responses_recorded_session_is_refused() {
    let turns = vec![WireTurn::Assistant(AssistantOutput::Responses(Vec::new()))];
    assert!(translate_completions(&turns, "", "claude-haiku-4-5").is_err());
}

#[test]
fn reasoning_format_follows_the_model_id() {
    assert_eq!(reasoning_format("google/gemini-3-pro"), "google-gemini-v1");
    assert_eq!(reasoning_format("claude-haiku-4-5"), "anthropic-claude-v1");
}
