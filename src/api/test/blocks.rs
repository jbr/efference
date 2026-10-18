//! Tests for block ordering and kinds: thinking, redacted thinking,
//! interleaved provider order, empty tool input, unknown and terminal
//! event types, in-stream errors, aborted partials, and post-finalization
//! failures.

use super::*;

/// A tool call with no `input_json_delta` fragments projects to `{}`, not a
/// parse error.
#[test]
fn empty_tool_input_is_empty_object() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"now"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ])
    .unwrap();
    match completion.assistant {
        Payload::Assistant { output, .. } => {
            let calls = output.tool_calls().unwrap();
            assert_eq!(calls[0].input, serde_json::json!({}));
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// A streamed thinking block is assembled into a leading `ContentBlock::Thinking`
/// (docs/transport.md, docs/transport.md): `thinking_delta` chunks concatenate,
/// the `signature_delta` is preserved verbatim (mandatory for replay), and the block leads
/// the turn — before the text block that follows it in the stream.
#[test]
fn assembles_thinking_block_leading_the_turn() {
    let completion = feed(&[
            ("message_start", r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":5,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"weighing "}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"the options"}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"signature_delta","signature":"SiG=="}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("content_block_start", r#"{"index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"text_delta","text":"here's the answer"}}"#),
            ("content_block_stop", r#"{"index":1}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();
    match completion.assistant {
        Payload::Assistant {
            output: AssistantOutput::Messages(blocks),
            ..
        } => {
            assert_eq!(
                blocks,
                vec![
                    MessagesOutputBlock::Thinking {
                        text: "weighing the options".into(),
                        signature: "SiG==".into(),
                    },
                    MessagesOutputBlock::Text("here's the answer".into()),
                ]
            );
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// Provider order is the record's order. A turn that interleaves text, thinking,
/// text, and a tool call assembles to exactly that sequence, and `project` copies
/// it onto the wire without regrouping — the log is a transcript of the provider's
/// structure, not a reconstruction from buckets (docs/log-and-projection.md).
#[test]
fn assembles_and_projects_interleaved_blocks_in_provider_order() {
    let completion = feed(&[
            ("message_start", r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":5,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"first"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("content_block_start", r#"{"index":1,"content_block":{"type":"thinking","thinking":""}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"signature_delta","signature":"SiG=="}}"#),
            ("content_block_stop", r#"{"index":1}"#),
            ("content_block_start", r#"{"index":2,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":2,"delta":{"type":"text_delta","text":"second"}}"#),
            ("content_block_stop", r#"{"index":2}"#),
            ("content_block_start", r#"{"index":3,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#),
            ("content_block_delta", r#"{"index":3,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}"#),
            ("content_block_stop", r#"{"index":3}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":6}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();
    let Payload::Assistant {
        output: AssistantOutput::Messages(blocks),
        excluded: None,
    } = &completion.assistant
    else {
        panic!(
            "expected a finalized Payload::Assistant, got {:?}",
            completion.assistant
        )
    };
    assert_eq!(
        *blocks,
        vec![
            MessagesOutputBlock::Text("first".into()),
            MessagesOutputBlock::Thinking {
                text: "hmm".into(),
                signature: "SiG==".into(),
            },
            MessagesOutputBlock::Text("second".into()),
            MessagesOutputBlock::ToolUse(ToolCall::new(
                "bash",
                "toolu_1",
                serde_json::json!({"command": "ls"}),
            )),
        ]
    );

    let log = vec![crate::log::LogEntry {
        seq: 0,
        turn: 0,
        at: crate::log::Timestamp(0),
        submission_id: None,
        source: None,
        payload: completion.assistant.clone(),
    }];
    let wire = crate::project::project(&log);
    let kinds: Vec<&str> = wire[0]
        .content
        .iter()
        .map(|block| match block {
            ApiBlock::Text { .. } => "text",
            ApiBlock::Thinking { .. } => "thinking",
            ApiBlock::ToolUse { .. } => "tool_use",
            other => panic!("unexpected wire block {other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["text", "thinking", "text", "tool_use"]);
}

/// A `redacted_thinking` block (opaque `data`, no text/signature) is preserved into a
/// leading `ContentBlock::RedactedThinking` for verbatim replay.
#[test]
fn assembles_redacted_thinking_block() {
    let completion = feed(&[
            ("message_start", r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":5,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"redacted_thinking","data":"ENCRYPTED=="}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();
    match completion.assistant {
        Payload::Assistant {
            output: AssistantOutput::Messages(blocks),
            ..
        } => assert_eq!(
            blocks,
            vec![MessagesOutputBlock::RedactedThinking {
                data: "ENCRYPTED==".into()
            }]
        ),
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// An unknown event type is loud, never silently dropped: it stops assembly and
/// the completion carries the why as its `upstream_failure` — record-only, retried,
/// never committed as a clean turn.
#[test]
fn unknown_event_type_is_loud() {
    let completion = feed(&[("surprise", "{}")]).unwrap();
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("surprise"), "{why}");
}

/// OpenRouter's skin terminates the stream with a trailing `[DONE]` sentinel
/// (labeled `event: data`) as OpenAI's wire does; Anthropic never sends one.
/// It must be recognized as terminal, not tripped as an unknown event — this is
/// the second thing that crashed the first OpenRouter turn after the null usage.
#[test]
fn done_sentinel_is_terminal_not_unknown() {
    let completion = feed(&[
            ("message_start", r#"{"message":{"model":"thinkingmachines/inkling","usage":{"input_tokens":1,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
            // The OpenRouter/OpenAI terminator — must not error.
            ("data", "[DONE]"),
        ])
        .unwrap();
    match completion.assistant {
        Payload::Assistant { output, .. } => {
            assert_eq!(output.text(), vec!["hi"]);
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// An in-stream `error` event (e.g. `overloaded_error` after a 200) surfaces its
/// full body as the completion's `upstream_failure`, with whatever content arrived
/// before it preserved — the partial is the only copy anywhere (docs/transport.md).
#[test]
fn in_stream_error_surfaces_body() {
    let completion = feed(&[
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"half an ans"}}"#,
        ),
        (
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
    ])
    .unwrap();
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("overloaded_error"), "{why}");
    match &completion.assistant {
        Payload::Assistant { output, .. } => {
            assert_eq!(output.text(), vec!["half an ans"]);
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// A stream that closes without `message_stop` / `[DONE]` is the upstream-abort
/// case: unmarked, such a completion commits as a clean turn and the daemon
/// settles idle, indistinguishable from the agent choosing silence. Marked, the
/// partial is preserved — here thinking alone, the shape that would otherwise
/// project as a dangling thinking block.
#[test]
fn aborted_stream_assembles_as_record_only_partial() {
    let completion = feed(&[
        ("message_start", r#"{"message":{"model":"stealth/ox-alpha","usage":{"input_tokens":5,"output_tokens":1}}}"#),
        ("content_block_start", r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"working through"}}"#),
        // …and the stream simply ends: no message_delta, no message_stop.
    ])
    .unwrap();
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("before finalizing"), "{why}");
    assert_eq!(completion.finish, None);
    match &completion.assistant {
        Payload::Assistant { output, .. } => {
            assert_eq!(
                output.reasoning_text().len(),
                1,
                "the partial thinking must survive"
            );
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// The `[DONE]` sentinel alone finalizes — OpenRouter labels it terminally and a
/// skin that omitted `message_stop` must not read as an abort.
#[test]
fn done_sentinel_alone_finalizes() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        ),
        ("data", "[DONE]"),
    ])
    .unwrap();
    assert_eq!(completion.upstream_failure(), None);
}

/// A failure recorded after finalization (a transport error draining the closed
/// stream, a stray trailing event) does not unmake the completion — the provider
/// stood behind it with a `stop_reason` and usage.
#[test]
fn failure_after_finalization_keeps_the_completion_clean() {
    let mut a = Assembler::new();
    for (ty, data) in [
        (
            "message_start",
            r#"{"message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ] {
        a.apply(Some(ty), data).unwrap();
    }
    a.fail("connection reset while draining".into());
    let completion = a.finish().unwrap();
    assert_eq!(completion.upstream_failure(), None);
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&crate::api::FinishReason::Finished)
    );
}

/// An abort can cut a tool input mid-fragment. The record-only entry keeps the raw
/// bytes as a JSON string rather than erroring the whole partial away — it is never
/// dispatched or projected, only audited.
#[test]
fn aborted_stream_keeps_partial_tool_input_unparsed() {
    let completion = feed(&[
        ("content_block_start", r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash"}}"#),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"cargo t"}}"#),
        // stream dies mid-input
    ])
    .unwrap();
    assert!(completion.upstream_failure().is_some());
    match &completion.assistant {
        Payload::Assistant { output, .. } => {
            let calls = output.tool_calls().unwrap();
            assert_eq!(
                calls[0].input,
                Value::String("{\"command\":\"cargo t".into())
            );
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// A Messages in-stream `error` is classed by its nested type (`api/failure.rs`):
/// overloaded is transient, a rejected credential is terminal.
#[test]
fn a_messages_error_event_is_classed_by_its_type() {
    let overloaded = feed(&[(
        "error",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    )])
    .unwrap();
    assert_eq!(overloaded.failure_class, FailureClass::Transient);
    let refused = feed(&[(
        "error",
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
    )])
    .unwrap();
    assert_eq!(refused.failure_class, FailureClass::Terminal);
    assert!(
        refused
            .terminal_failure()
            .unwrap()
            .contains("authentication_error")
    );
}
