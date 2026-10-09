use super::*;
use crate::log::ToolCall;

/// Feed a recorded SSE capture through the assembler, collecting the live view.
fn assemble(capture: &str) -> (Completion, Vec<Streamed>) {
    let mut assembler = CompletionsAssembler::new();
    let mut live = Vec::new();
    for line in capture.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            live.extend(assembler.apply(None, data).unwrap());
        }
    }
    (assembler.finish().unwrap(), live)
}

fn message(completion: &Completion) -> &CompletionsMessage {
    match completion.assistant_output() {
        AssistantOutput::Completions(message) => message,
        other => panic!("not a Completions turn: {other:?}"),
    }
}

fn failure(completion: &Completion) -> Option<&UpstreamFailureRecord> {
    match &completion.assistant {
        Payload::Assistant {
            upstream_failure, ..
        } => upstream_failure.as_ref(),
        _ => None,
    }
}

/// OpenRouter repeats `role`, `type`, and `format` in every fragment; they must
/// not concatenate. The signature arrives in its own fragment at the same index
/// and joins the text detail; the arguments arrive split after an empty first
/// fragment.
#[test]
fn openrouter_claude_assembles_reasoning_details_and_split_arguments() {
    let (completion, live) = assemble(include_str!("../openrouter_claude.sse"));
    let stored = message(&completion);
    let map = stored.as_map();
    assert!(map.get("role").is_none());
    let details = map["reasoning_details"].as_array().unwrap();
    assert_eq!(details.len(), 1);
    assert_eq!(details[0]["type"], "reasoning.text");
    assert_eq!(details[0]["format"], "anthropic-claude-v1");
    assert!(
        details[0]["signature"]
            .as_str()
            .unwrap()
            .starts_with("EqIH")
    );
    assert_eq!(
        details[0]["text"].as_str(),
        map["reasoning"].as_str(),
        "the detail text and the flat reasoning are the same stream"
    );
    let calls = stored.tool_calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.0, "toolu_bdrk_01BngBQJT9qRCMfY7phEkZus");
    assert!(
        calls[0].input["command"]
            .as_str()
            .unwrap()
            .contains("poem.txt")
    );
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::ToolCalls)
    );
    assert_eq!(
        completion.receipt.served_by.as_deref(),
        Some("Amazon Bedrock")
    );
    assert!(completion.receipt.cost.is_some());
    assert_eq!(completion.usage.output_tokens, 335);
    assert!(failure(&completion).is_none());
    assert!(matches!(live.first(), Some(Streamed::Thinking { .. })));
    assert!(live.iter().any(|s| matches!(s, Streamed::ToolStart { .. })));
}

/// Gemini's thought signature is a `reasoning.encrypted` detail at the same
/// index as the readable one, and stays a separate element: it is what replay
/// must return intact.
#[test]
fn openrouter_gemini_keeps_the_signature_detail_separate() {
    let (completion, _) = assemble(include_str!("../openrouter_gemini.sse"));
    let stored = message(&completion);
    let details = stored.as_map()["reasoning_details"].as_array().unwrap();
    let kinds: Vec<_> = details
        .iter()
        .map(|d| d["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["reasoning.text", "reasoning.encrypted"]);
    assert_eq!(details[1]["id"], "tool_bash_NmdhJwj4jdLZLHlS0SH5");
    let calls = stored.tool_calls().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "the typeless argument fragment joins its call"
    );
    assert_eq!(calls[0].id.0, "tool_bash_NmdhJwj4jdLZLHlS0SH5");
}

/// Mistral's reasoning arrives as index-less `thinking` parts, then the answer
/// as plain strings: one thinking part and one text part.
#[test]
fn mistral_thinking_parts_continue_and_the_answer_becomes_a_text_part() {
    let (completion, live) = assemble(include_str!("../mistral_reasoning.sse"));
    let stored = message(&completion);
    let parts = stored.as_map()["content"].as_array().unwrap();
    let kinds: Vec<_> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["thinking", "text"]);
    assert_eq!(parts[0]["closed"], true);
    assert_eq!(parts[0]["thinking"].as_array().unwrap().len(), 1);
    assert!(stored.reasoning_text()[0].starts_with("The user wants me to create"));
    assert!(stored.text()[0].contains("cat > /tmp/poem.txt"));
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::Finished)
    );
    assert!(live.iter().any(|s| matches!(s, Streamed::Thinking { .. })));
    assert!(live.iter().any(|s| matches!(s, Streamed::Text { .. })));
}

/// Reasoning and a call in one turn: the thinking part is the whole `content`,
/// and the call beside it. Mistral accepted this message replayed verbatim and
/// counted the thinking in the prompt (backlog/completions-dialect/dialect.md).
#[test]
fn mistral_thinking_beside_a_tool_call() {
    let (completion, _) = assemble(include_str!("../mistral_reasoning_tool.sse"));
    let stored = message(&completion);
    let parts = stored.as_map()["content"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["type"], "thinking");
    assert!(!stored.reasoning_text().is_empty());
    assert_eq!(stored.tool_calls().unwrap().len(), 1);
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::ToolCalls)
    );
}

/// Two parallel calls whole in one chunk; usage rides the same chunk.
#[test]
fn mistral_parallel_calls_in_one_chunk() {
    let (completion, _) = assemble(include_str!("../mistral_tool.sse"));
    let calls = message(&completion).tool_calls().unwrap();
    assert_eq!(
        calls,
        vec![
            ToolCall::new(
                "bash",
                "8ufwVNVUd",
                serde_json::json!({"command": "ls -l /tmp"})
            ),
            ToolCall::new(
                "bash",
                "bjAcfMBPM",
                serde_json::json!({"command": "ls -l /var"})
            ),
        ]
    );
    assert_eq!(completion.usage.input_tokens, 91);
    assert_eq!(completion.usage.output_tokens, 27);
}

#[test]
fn mistral_plain_text() {
    let (completion, live) = assemble(include_str!("../mistral_text.sse"));
    let stored = message(&completion);
    assert_eq!(stored.text(), vec!["Hello there! How are you?"]);
    assert_eq!(
        serde_json::to_value(stored).unwrap(),
        serde_json::json!({"content": "Hello there! How are you?"})
    );
    assert_eq!(live.len(), 4);
}

/// DeepSeek's `reasoning_content` arrives beside `"content": null`, which must
/// not erase the answer.
#[test]
fn deepseek_reasoning_content_and_nulls() {
    let (completion, _) = assemble(include_str!("../deepseek_flash_tool.sse"));
    let stored = message(&completion);
    let map = stored.as_map();
    assert!(
        map["reasoning_content"]
            .as_str()
            .unwrap()
            .starts_with("The user wants")
    );
    assert!(map["content"].as_str().unwrap().starts_with("I'll compose"));
    assert_eq!(stored.tool_calls().unwrap().len(), 1);
    let (completion, _) = assemble(include_str!("../deepseek_pro_tool.sse"));
    let ids: Vec<_> = message(&completion)
        .tool_calls()
        .unwrap()
        .into_iter()
        .map(|call| call.id.0)
        .collect();
    assert_eq!(
        ids,
        [
            "call_00_KjKneXiYX4zsO45fAXFK0066",
            "call_01_eWvCJQmPmBOhEL04xwNE3985"
        ]
    );
}

/// Inception sends `"tool_calls": null` beside text and reports cached tokens.
#[test]
fn inception_nulls_and_cached_tokens() {
    let (completion, _) = assemble(include_str!("../inception_text.sse"));
    assert_eq!(
        serde_json::to_value(message(&completion)).unwrap(),
        serde_json::json!({"content": "Hello, how are you today?"})
    );
    assert_eq!(completion.usage.cache_read_input_tokens, 3);
    assert_eq!(completion.usage.input_tokens, 6);
    let (completion, _) = assemble(include_str!("../inception_tool.sse"));
    assert_eq!(message(&completion).tool_calls().unwrap().len(), 1);
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::ToolCalls)
    );
}

/// A stream cut before any finish is record-only.
#[test]
fn an_unfinished_stream_is_record_only() {
    let capture = include_str!("../mistral_text.sse");
    let cut: String = capture.lines().take(3).collect::<Vec<_>>().join("\n");
    let (completion, _) = assemble(&cut);
    assert!(failure(&completion).is_some());
}

#[test]
fn a_mid_stream_error_is_an_error() {
    let mut assembler = CompletionsAssembler::new();
    let error = assembler
        .apply(
            None,
            r#"{"error":{"message":"overloaded","type":"server_error","code":503}}"#,
        )
        .unwrap_err();
    assert!(error.to_string().contains("overloaded"));
}
