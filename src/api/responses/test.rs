use super::assembler::ResponsesAssembler;
use super::wire::{FunctionOutput, InputContent, InputItem, InputRole};
use super::*;
use crate::api::provider::PromptCachePolicy;
use crate::api::{Completion, Streamed};
use crate::log::{
    AssistantOutput, CallId, Payload, ResponsesMessageContent, ResponsesOutputItem, ResponsesPhase,
    ResponsesReasoningSummary, ResultImage,
};
use crate::project::{UserPart, WireTurn};

fn user(text: &str) -> WireTurn {
    WireTurn::text(text)
}

fn result(call_id: &str, text: &str) -> UserPart {
    UserPart::ToolResult {
        call_id: CallId(call_id.into()),
        seq: Some(3),
        content: text.into(),
        is_error: false,
        images: Vec::new(),
    }
}

fn responses_turn() -> WireTurn {
    WireTurn::Assistant(AssistantOutput::Responses(vec![
        ResponsesOutputItem::Reasoning {
            id: "rs_1".into(),
            status: Some("completed".into()),
            summary: vec![ResponsesReasoningSummary {
                text: "thinking".into(),
            }],
            content: Vec::new(),
            encrypted_content: Some("ENC==".into()),
        },
        ResponsesOutputItem::Message {
            id: "msg_1".into(),
            status: "completed".into(),
            phase: Some(ResponsesPhase::Commentary),
            content: vec![ResponsesMessageContent::OutputText {
                text: "on it".into(),
                annotations: Vec::new(),
            }],
        },
        ResponsesOutputItem::FunctionCall {
            id: Some("fc_1".into()),
            call_id: CallId("call_1".into()),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
            status: Some("completed".into()),
        },
    ]))
}

/// The whole envelope, byte for byte — the Responses analogue of
/// `request_envelope_matches_expected_wire_bytes`. A reorder or rename here moves
/// the production cache, so it must break this first.
#[test]
fn responses_envelope_matches_expected_wire_bytes() {
    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run a command",
        "input_schema": {"type": "object"},
    })];
    let mut turns = vec![user("hello")];
    turns.push(WireTurn::Breakpoint);
    let request = ResponsesRequest {
        model: "gpt-5.6-luna",
        instructions: None,
        input: encode_responses(&turns, "you are a harness", PromptCachePolicy::Explicit),
        tools: tool_definitions(&tools),
        tool_choice: tool_choice_value(&ToolChoice::auto()),
        parallel_tool_calls: false,
        store: false,
        include: None,
        truncation: Some("disabled"),
        max_output_tokens: Some(4096),
        reasoning: Some(Reasoning {
            effort: Some("low".into()),
            summary: "auto",
        }),
        temperature: Some(0.7),
        prompt_cache_key: Some("sess"),
        prompt_cache_options: Some(PromptCacheOptions { mode: "explicit" }),
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"gpt-5.6-luna","#,
        r#""input":[{"type":"message","role":"developer","content":[{"type":"input_text","text":"you are a harness"}]},"#,
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"hello","prompt_cache_breakpoint":{"mode":"explicit"}}]}],"#,
        r#""tools":[{"description":"run a command","name":"bash","parameters":{"type":"object"},"strict":false,"type":"function"}],"#,
        r#""tool_choice":"auto","parallel_tool_calls":false,"store":false,"truncation":"disabled","max_output_tokens":4096,"#,
        r#""reasoning":{"effort":"low","summary":"auto"},"temperature":0.7,"prompt_cache_key":"sess","#,
        r#""prompt_cache_options":{"mode":"explicit"},"stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// The Codex-backend profile: the preamble rides `instructions`, `include` names
/// the encrypted reasoning, and the fields Codex CLI never sends
/// (`max_output_tokens`, `truncation`, `temperature`, `prompt_cache_options`) are
/// absent. Pinned like the platform envelope.
#[test]
fn codex_profile_envelope_matches_expected_wire_bytes() {
    let turns = vec![user("hello")];
    let request = ResponsesRequest {
        model: "gpt-5.6-luna",
        instructions: Some("you are a harness"),
        input: encode_responses(&turns, "", PromptCachePolicy::None),
        tools: Vec::new(),
        tool_choice: tool_choice_value(&ToolChoice::auto()),
        parallel_tool_calls: false,
        store: false,
        include: Some(vec!["reasoning.encrypted_content"]),
        truncation: None,
        max_output_tokens: None,
        reasoning: Some(Reasoning {
            effort: None,
            summary: "auto",
        }),
        temperature: None,
        prompt_cache_key: Some("sess"),
        prompt_cache_options: None,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"gpt-5.6-luna","instructions":"you are a harness","#,
        r#""input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}],"#,
        r#""tools":[],"tool_choice":"auto","parallel_tool_calls":false,"store":false,"#,
        r#""include":["reasoning.encrypted_content"],"reasoning":{"summary":"auto"},"#,
        r#""prompt_cache_key":"sess","stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// Results answer as `function_call_output` items ahead of the prose that follows
/// them, and a logged Responses turn replays as the items the provider emitted —
/// reasoning with its opaque continuation, message with its phase, call with its
/// exact argument string.
#[test]
fn encodes_results_then_prose_and_replays_items_verbatim() {
    let turns = vec![
        user("go"),
        responses_turn(),
        WireTurn::User(vec![
            result("call_1", "[3] a\nb"),
            UserPart::Text("now what".into()),
        ]),
    ];
    let input = encode_responses(&turns, "", PromptCachePolicy::Implicit);
    let json = serde_json::to_value(&input).unwrap();
    let kinds: Vec<&str> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "message",
            "reasoning",
            "message",
            "function_call",
            "function_call_output",
            "message"
        ]
    );
    assert_eq!(json[1]["encrypted_content"], "ENC==");
    assert_eq!(json[1]["summary"][0]["type"], "summary_text");
    assert_eq!(json[2]["phase"], "commentary");
    assert_eq!(json[2]["role"], "assistant");
    assert_eq!(json[3]["arguments"], r#"{"command":"ls"}"#);
    assert_eq!(json[3]["call_id"], "call_1");
    assert_eq!(json[4]["call_id"], "call_1");
    assert_eq!(json[4]["output"], "[3] a\nb");
    assert_eq!(json[5]["role"], "user");
    // No marker under implicit policy, anywhere.
    assert!(
        !serde_json::to_string(&input)
            .unwrap()
            .contains("prompt_cache_breakpoint")
    );
}

/// A result with images takes the parts form; text results stay a bare string so
/// the common case is the smallest wire.
#[test]
fn image_results_take_the_parts_form() {
    let turns = vec![WireTurn::User(vec![UserPart::ToolResult {
        call_id: CallId("c".into()),
        seq: Some(1),
        content: "[1] shot".into(),
        is_error: false,
        images: vec![ResultImage {
            media_type: "image/png".into(),
            data: "AAAA".into(),
            width: 1,
            height: 1,
        }],
    }])];
    let input = encode_responses(&turns, "", PromptCachePolicy::Implicit);
    match &input[0] {
        InputItem::FunctionCallOutput {
            output: FunctionOutput::Parts(parts),
            ..
        } => {
            assert!(
                matches!(&parts[0], InputContent::InputText { text, .. } if text == "[1] shot")
            );
            assert!(
                matches!(&parts[1], InputContent::InputImage { image_url, .. } if image_url == "data:image/png;base64,AAAA")
            );
        }
        other => panic!("expected parts output, got {other:?}"),
    }
}

/// The trailing marker walks back past assistant items (not carriers) to the last
/// user part — the physical marker sits earlier than the logical frontier — and a
/// bare-string result is lifted to parts to carry it.
#[test]
fn breakpoint_walks_back_to_the_last_eligible_carrier() {
    let turns = vec![
        WireTurn::User(vec![result("c0", "[1] out")]),
        responses_turn(),
        WireTurn::Breakpoint,
    ];
    let input = encode_responses(&turns, "", PromptCachePolicy::Explicit);
    let json = serde_json::to_value(&input).unwrap();
    assert_eq!(json[0]["type"], "function_call_output");
    assert_eq!(
        json[0]["output"][0]["prompt_cache_breakpoint"]["mode"],
        "explicit"
    );
    assert_eq!(json[0]["output"][0]["text"], "[1] out");
    let text = serde_json::to_string(&input).unwrap();
    assert_eq!(text.matches("prompt_cache_breakpoint").count(), 1);
}

/// A mid-turn marker (the spotter's stable scope block) lands on the part before it
/// and the prose after it stays in the same message.
#[test]
fn mid_turn_breakpoint_marks_the_preceding_part() {
    let turns = vec![WireTurn::User(vec![
        UserPart::Text("scope".into()),
        UserPart::Breakpoint,
        UserPart::Text("move".into()),
    ])];
    let input = encode_responses(&turns, "", PromptCachePolicy::Explicit);
    let InputItem::Message { role, content } = &input[0] else {
        panic!("expected one message");
    };
    assert_eq!(*role, InputRole::User);
    assert_eq!(content.len(), 2);
    assert!(matches!(
        &content[0],
        InputContent::InputText {
            prompt_cache_breakpoint: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &content[1],
        InputContent::InputText {
            prompt_cache_breakpoint: None,
            ..
        }
    ));
}

fn feed(events: &[&str]) -> (Completion, Vec<Streamed>) {
    let mut a = ResponsesAssembler::new();
    let mut streamed = Vec::new();
    for data in events {
        match a.apply(None, data) {
            Ok(Some(s)) => streamed.push(s),
            Ok(None) => {}
            Err(e) => a.fail(e.to_string()),
        }
    }
    (a.finish().unwrap(), streamed)
}

const COMPLETED_WITH_CALL: &str = r#"{"type":"response.completed","response":{"id":"resp_1","model":"gpt-5.6-luna","status":"completed","output":[
  {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"FULL=="},
  {"type":"message","id":"msg_1","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"running","annotations":[]}]},
  {"type":"function_call","id":"fc_1","call_id":"call_1","name":"bash","arguments":"{\"command\":\"ls\"}","status":"completed"}
],"usage":{"input_tokens":1000,"output_tokens":20,"input_tokens_details":{"cached_tokens":600,"cache_write_tokens":100},"output_tokens_details":{"reasoning_tokens":5}}}}"#;

/// The terminal event's `output` is the committed turn, in order, with the
/// complete `encrypted_content` (the `added` copy may be partial); deltas feed the
/// live view with the output index. Usage is restated so the three-way split sums
/// to the prompt total rather than double-counting cached input.
#[test]
fn assembles_a_completed_turn_from_the_terminal_output() {
    let (completion, streamed) = feed(&[
        r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5.6-luna","status":"in_progress","output":[]}}"#,
        r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"PART"}}"#,
        r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"rs_1","summary_index":0,"delta":"pl"}"#,
        r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"FULL=="}}"#,
        r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"msg_1","role":"assistant","status":"in_progress","content":[]}}"#,
        r#"{"type":"response.output_text.delta","output_index":1,"item_id":"msg_1","content_index":0,"delta":"run"}"#,
        r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"bash","arguments":"","status":"in_progress"}}"#,
        r#"{"type":"response.function_call_arguments.delta","output_index":2,"item_id":"fc_1","delta":"{\"com"}"#,
        COMPLETED_WITH_CALL,
    ]);
    assert_eq!(
        streamed,
        vec![
            Streamed::Thinking {
                index: 0,
                text: "pl".into()
            },
            Streamed::Text {
                index: 1,
                text: "run".into()
            },
            Streamed::ToolStart {
                index: 2,
                id: "call_1".into(),
                name: "bash".into()
            },
            Streamed::ToolInput {
                index: 2,
                fragment: "{\"com".into()
            },
        ]
    );
    let Payload::Assistant {
        output: AssistantOutput::Responses(items),
        upstream_failure: None,
    } = &completion.assistant
    else {
        panic!(
            "expected a finalized Responses turn: {:?}",
            completion.assistant
        );
    };
    assert_eq!(items.len(), 3);
    assert!(
        matches!(&items[0], ResponsesOutputItem::Reasoning { encrypted_content: Some(e), .. } if e == "FULL==")
    );
    assert!(matches!(
        &items[1],
        ResponsesOutputItem::Message {
            phase: Some(ResponsesPhase::Commentary),
            ..
        }
    ));
    let calls = completion.assistant_output().tool_calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.0, "call_1");
    assert_eq!(calls[0].input, serde_json::json!({"command": "ls"}));
    assert_eq!(completion.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(completion.id.as_deref(), Some("resp_1"));
    assert_eq!(completion.usage.input_tokens, 300);
    assert_eq!(completion.usage.cache_read_input_tokens, 600);
    assert_eq!(completion.usage.cache_creation_input_tokens, 100);
    assert_eq!(completion.usage.output_tokens, 20);
}

/// No terminal event: record-only, partial kept.
#[test]
fn a_stream_without_a_terminal_event_is_record_only() {
    let (completion, _) = feed(&[
        r#"{"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}"#,
        r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"m","role":"assistant","status":"completed","content":[{"type":"output_text","text":"half"}]}}"#,
    ]);
    assert!(
        completion
            .upstream_failure()
            .is_some_and(|why| why.contains("no response.completed")),
        "{:?}",
        completion.upstream_failure()
    );
    assert_eq!(completion.assistant_output().text(), vec!["half"]);
}

/// An in-stream `error` event ends assembly as a record-only partial with the
/// provider's message.
#[test]
fn an_error_event_marks_the_turn_record_only() {
    let (completion, _) = feed(&[
        r#"{"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}"#,
        r#"{"type":"error","code":"server_error","message":"boom","param":null}"#,
    ]);
    assert!(
        completion
            .upstream_failure()
            .is_some_and(|why| why.contains("boom"))
    );
}

/// The live quota error nests its fields under `error`; the reference's flat shape
/// is read too. The code rides the message so a log line names the class.
#[test]
fn a_nested_error_event_surfaces_code_and_message() {
    let (completion, _) = feed(&[
        r#"{"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}"#,
        r#"{"type":"error","error":{"type":"insufficient_quota","code":"credit_balance_exhausted","message":"You have no credits remaining.","param":null},"sequence_number":2}"#,
    ]);
    let why = completion.upstream_failure().unwrap();
    assert!(why.contains("[credit_balance_exhausted]"), "{why}");
    assert!(why.contains("You have no credits remaining."), "{why}");
}

/// `incomplete` at the output cap normalizes to the Messages spelling the loop
/// reads; a call cut by the cap stays undispatched.
#[test]
fn incomplete_at_the_cap_normalizes_to_max_tokens() {
    let (completion, _) = feed(&[
        r#"{"type":"response.incomplete","response":{"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","id":"m","role":"assistant","status":"incomplete","content":[{"type":"output_text","text":"so far"}]}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    ]);
    assert_eq!(completion.stop_reason.as_deref(), Some("max_tokens"));
    assert!(completion.upstream_failure().is_none());

    let (cut, _) = feed(&[
        r#"{"type":"response.incomplete","response":{"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"c","name":"bash","arguments":"{\"command\":\"ls\"}"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    ]);
    assert!(
        cut.upstream_failure()
            .is_some_and(|why| why.contains("output-token cap"))
    );
}

/// An output item outside the handled set is kept whole for audit and makes the
/// turn record-only rather than dispatching or replaying half a response.
#[test]
fn an_unsupported_item_is_kept_and_forces_record_only() {
    let (completion, _) = feed(&[
        r#"{"type":"response.completed","response":{"id":"r","status":"completed","output":[{"type":"web_search_call","id":"ws_1","status":"completed"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    ]);
    assert!(
        completion
            .upstream_failure()
            .is_some_and(|why| why.contains("web_search_call"))
    );
    let Payload::Assistant {
        output: AssistantOutput::Responses(items),
        ..
    } = &completion.assistant
    else {
        panic!()
    };
    assert!(matches!(&items[0], ResponsesOutputItem::Unsupported(v) if v["id"] == "ws_1"));
}

/// Malformed arguments make the turn record-only; the exact string is kept.
#[test]
fn malformed_arguments_force_record_only() {
    let (completion, _) = feed(&[
        r#"{"type":"response.completed","response":{"id":"r","status":"completed","output":[{"type":"function_call","call_id":"c","name":"bash","arguments":"{not json"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    ]);
    assert!(
        completion
            .upstream_failure()
            .is_some_and(|why| why.contains("unparseable arguments"))
    );
    assert!(
        completion.assistant_output().tool_calls().is_err(),
        "the derived view refuses what the record keeps"
    );
}

/// A logged Responses turn round-trips through the log's own serialization.
#[test]
fn a_responses_turn_round_trips_through_the_log() {
    let WireTurn::Assistant(output) = responses_turn() else {
        unreachable!()
    };
    let line = serde_json::to_string(&output).unwrap();
    let back: AssistantOutput = serde_json::from_str(&line).unwrap();
    assert_eq!(back, output);
}
