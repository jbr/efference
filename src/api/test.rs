use super::client::record_upstream_failure;
use super::thinking::is_recognized_adaptive_claude;
use super::*;

/// A bare `ANTHROPIC_API_KEY` and no `EFFERENCE_PROVIDER` resolves to the
/// Anthropic provider: its endpoint, `x-api-key` auth, key verbatim.
#[test]
fn endpoint_defaults_to_anthropic_from_bare_key() {
    let e = Endpoint::resolve(None, None, Some("sk-ant-xyz".into())).unwrap();
    assert_eq!(e.provider, Provider::Anthropic);
    assert_eq!(e.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(e.auth_header, "x-api-key");
    assert_eq!(e.auth_value, "sk-ant-xyz");
}

/// One declared word derives the whole OpenRouter endpoint: URL,
/// `authorization` header, `Bearer ` prefix. The secret arrives alone and
/// stays a *whole* value (an `op://…` reference `op run` resolves).
#[test]
fn endpoint_openrouter_derived_from_declared_provider() {
    let e = Endpoint::resolve(
        Some("openrouter".into()),
        Some("sk-or-v1-abc".into()),
        None,
    )
    .unwrap();
    assert_eq!(e.provider, Provider::OpenRouter);
    assert_eq!(e.url, "https://openrouter.ai/api/v1/messages");
    assert_eq!(e.auth_header, "authorization");
    assert_eq!(e.auth_value, "Bearer sk-or-v1-abc");
}

/// `EFFERENCE_API_KEY` wins over a lingering `ANTHROPIC_API_KEY`, and rides
/// through verbatim (base64 `=` padding intact).
#[test]
fn endpoint_api_key_overrides_anthropic_key() {
    let e = Endpoint::resolve(None, Some("abc==".into()), Some("sk-ant".into())).unwrap();
    assert_eq!(e.auth_header, "x-api-key");
    assert_eq!(e.auth_value, "abc==");
}

/// The `ANTHROPIC_API_KEY` fallback covers the Anthropic provider only — an
/// Anthropic key at OpenRouter would 401 confusingly, so it errors at resolve,
/// naming the variable to set.
#[test]
fn endpoint_openrouter_rejects_anthropic_key_fallback() {
    let err = Endpoint::resolve(Some("openrouter".into()), None, Some("sk-ant".into()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("EFFERENCE_API_KEY"), "{err}");
}

/// An unrecognized provider is a hard error naming the valid values — never a
/// silent fallback to a host the key doesn't belong to.
#[test]
fn endpoint_unknown_provider_is_loud() {
    let err = Endpoint::resolve(Some("openai".into()), Some("k".into()), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("anthropic"), "{err}");
    assert!(err.contains("openrouter"), "{err}");
}

/// A blank `EFFERENCE_PROVIDER` means unset, not an unknown provider.
#[test]
fn endpoint_blank_provider_defaults_to_anthropic() {
    let e = Endpoint::resolve(Some("  ".into()), Some("k".into()), None).unwrap();
    assert_eq!(e.provider, Provider::Anthropic);
}

/// No credentials at all is an error naming both ways to provide them.
#[test]
fn endpoint_errors_with_no_credentials() {
    assert!(Endpoint::resolve(None, None, None).is_err());
}

/// The Anthropic Models API shape: `max_input_tokens` is the context window,
/// `max_tokens` the output cap; unknown fields (`capabilities`, …) ignored.
#[test]
fn anthropic_capabilities_parse_models_api_shape() {
    let body = r#"{
        "id": "claude-haiku-4-5",
        "display_name": "Claude Haiku 4.5",
        "created_at": "2025-10-01T00:00:00Z",
        "type": "model",
        "max_input_tokens": 200000,
        "max_tokens": 64000,
        "capabilities": {"image_input": {"supported": true}}
    }"#;
    let caps = Provider::Anthropic.parse_capabilities(body).unwrap();
    assert_eq!(caps.context_length, 200000);
    assert_eq!(caps.max_output_tokens, Some(64000));
}

/// A Models API response without `max_input_tokens` fails the (required) boot
/// lookup rather than defaulting — the ceiling would otherwise be a guess.
#[test]
fn anthropic_capabilities_missing_context_is_an_error() {
    let err = Provider::Anthropic
        .parse_capabilities(r#"{"id": "m", "display_name": "M"}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("max_input_tokens"), "{err}");
}

/// The OpenRouter endpoints shape: `context_length` is per serving endpoint
/// (routing is unpinned), so the effective ceiling is the **min** across them;
/// likewise `max_completion_tokens`. Trimmed from a live
/// `GET /api/v1/models/anthropic/claude-haiku-4.5/endpoints` response.
#[test]
fn openrouter_capabilities_take_min_across_endpoints() {
    let body = r#"{
        "data": {
            "id": "anthropic/claude-haiku-4.5",
            "architecture": {"input_modalities": ["text", "image", "file"]},
            "endpoints": [
                {"provider_name": "Google", "context_length": 200000,
                 "max_completion_tokens": 64000, "supports_implicit_caching": false},
                {"provider_name": "Anthropic", "context_length": 195000,
                 "max_completion_tokens": 32000, "supports_implicit_caching": false},
                {"provider_name": "Azure", "context_length": null,
                 "max_completion_tokens": null, "supports_implicit_caching": false}
            ]
        }
    }"#;
    let caps = Provider::OpenRouter.parse_capabilities(body).unwrap();
    assert_eq!(caps.context_length, 195000);
    assert_eq!(caps.max_output_tokens, Some(32000));
}

/// A model no endpoint currently serves (or that reports no context length at
/// all) fails the boot lookup with a legible message.
#[test]
fn openrouter_capabilities_no_endpoints_is_an_error() {
    let err = Provider::OpenRouter
        .parse_capabilities(r#"{"data": {"endpoints": []}}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("context_length"), "{err}");
}

/// OpenRouter's generation record prices one request authoritatively —
/// `total_cost` is what the account was billed (docs/transport.md). Unknown
/// fields ride through; a record without the field is an error the daemon
/// counts as unpriced, never a silent zero.
#[test]
fn generation_cost_parses_total_cost() {
    let body = r#"{
        "data": {
            "id": "gen-abc123",
            "model": "anthropic/claude-haiku-4.5",
            "total_cost": 0.0421,
            "cache_discount": 0.011,
            "tokens_prompt": 9407,
            "tokens_completion": 120
        }
    }"#;
    assert_eq!(provider::parse_generation_cost(body).unwrap(), 0.0421);

    let err = provider::parse_generation_cost(r#"{"data": {"id": "gen-x"}}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("total_cost"), "{err}");
}

/// OpenRouter model ids are `author/slug`; a slash-less id can't address the
/// metadata route and errors before any request is made.
#[test]
fn openrouter_capabilities_url_requires_author_slug() {
    let err = Provider::OpenRouter
        .capabilities_url("claude-haiku-4-5")
        .unwrap_err()
        .to_string();
    assert!(err.contains("author/slug"), "{err}");
    let url = Provider::OpenRouter
        .capabilities_url("anthropic/claude-haiku-4.5")
        .unwrap();
    assert_eq!(
        url,
        "https://openrouter.ai/api/v1/models/anthropic/claude-haiku-4.5/endpoints"
    );
}

/// The `auto` thinking default is a Claude allowlist, not "everything but haiku":
/// foreign models (reached via OpenRouter) never get an Anthropic `thinking` block,
/// while direct ids and `anthropic/…` slugs do — except Haiku.
#[test]
fn adaptive_thinking_allowlist_is_claude_non_haiku() {
    assert!(is_recognized_adaptive_claude("claude-opus-4-8"));
    assert!(is_recognized_adaptive_claude("claude-sonnet-5"));
    assert!(is_recognized_adaptive_claude("anthropic/claude-opus-4.1"));
    assert!(is_recognized_adaptive_claude("claude-fable-5"));
    assert!(!is_recognized_adaptive_claude("claude-haiku-4-5-20251001"));
    assert!(!is_recognized_adaptive_claude("anthropic/claude-haiku-4.5"));
    assert!(!is_recognized_adaptive_claude("openai/gpt-5.6"));
    assert!(!is_recognized_adaptive_claude("google/gemini-2.5-pro"));
    assert!(!is_recognized_adaptive_claude("deepseek/deepseek-chat"));
}

fn feed(events: &[(&str, &str)]) -> Result<Completion, Error> {
    let mut a = Assembler::new();
    for (ty, data) in events {
        a.apply(Some(ty), data)?;
    }
    a.finish()
}

/// Everything the sink sees, in order — the live channel, as distinct from the assembled
/// [`Completion`] that `feed` returns.
fn live(events: &[(&str, &str)]) -> Vec<Streamed> {
    let mut a = Assembler::new();
    let mut out = Vec::new();
    for (ty, data) in events {
        if let Some(s) = a.apply(Some(ty), data).unwrap() {
            out.push(s);
        }
    }
    out
}

/// **Every generation phase reaches the live sink**, not just prose
/// (docs/transport.md). The sink used to carry a bare text fragment, so a turn
/// that thought and then emitted a tool call published *nothing* until prose arrived — and a
/// turn with no prose at all published nothing, full stop, which is indistinguishable from a
/// hung harness. The tool call is named at `content_block_start`, before its arguments,
/// because a large input can take seconds to stream.
#[test]
fn every_phase_reaches_the_live_sink_not_just_prose() {
    let seen = live(&[
        (
            "message_start",
            r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"weighing "}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"the options"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"ls\"}"}}"#,
        ),
        ("content_block_stop", r#"{"index":1}"#),
    ]);
    assert_eq!(
        seen,
        vec![
            Streamed::Thinking("weighing ".into()),
            Streamed::Thinking("the options".into()),
            // The call is named before a single argument byte arrives.
            Streamed::ToolStart {
                name: "bash".into()
            },
            Streamed::ToolInput("{\"comm".into()),
            Streamed::ToolInput("and\":\"ls\"}".into()),
        ],
        "a thinking-then-tool-call turn must not stream silently"
    );
    // A signature delta carries no live signal — it's replay metadata, not progress.
    assert!(!seen.iter().any(|s| matches!(s, Streamed::Text(_))));
}

/// Consecutive equal statuses increment the diagnostic counter; a different
/// status starts a new run (docs/transport.md).
#[test]
fn upstream_failure_counts_consecutive_statuses() {
    let failure = upstream_failure_state();

    record_upstream_failure(&failure, 429);
    record_upstream_failure(&failure, 429);

    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure {
            status: 429,
            count: 2,
        })
    );
    record_upstream_failure(&failure, 503);
    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure {
            status: 503,
            count: 1,
        })
    );
}

/// Prose still streams as before, and is typed as prose — the regression guard on the
/// existing live view, which is what the sink carried before it was typed.
#[test]
fn prose_still_reaches_the_sink_as_text() {
    let seen = live(&[
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"let me "}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"look"}}"#,
        ),
    ]);
    assert_eq!(
        seen,
        vec![
            Streamed::Text("let me ".into()),
            Streamed::Text("look".into())
        ]
    );
}

/// The path the bash tool depends on: a text block plus a tool call whose
/// JSON input is **split across three `input_json_delta` fragments** — the
/// reassembly that makes streaming non-trivial. Also pins the usage merge:
/// `cache_read` from `message_start`, final `output_tokens` from
/// `message_delta`.
#[test]
fn assembles_text_and_split_tool_use() {
    let completion = feed(&[
            ("message_start", r#"{"type":"message_start","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":9407}}}"#),
            ("ping", r#"{"type":"ping"}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"let me look"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash"}}"#),
            // One JSON object, three arbitrary byte splits.
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"l"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"s\"}"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();

    assert_eq!(completion.model, "claude-haiku-4-5");
    assert_eq!(completion.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(completion.usage.input_tokens, 3);
    assert_eq!(completion.usage.cache_read_input_tokens, 9407); // message_start
    assert_eq!(completion.usage.output_tokens, 4); // message_delta, not the placeholder 1

    match completion.assistant {
        Payload::Assistant {
            thinking,
            text,
            calls,
        } => {
            assert!(thinking.is_empty(), "no thinking blocks in this stream");
            assert_eq!(text, vec![ContentBlock::Text("let me look".into())]);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "bash");
            assert_eq!(calls[0].id.0, "toolu_9");
            assert_eq!(calls[0].input, serde_json::json!({ "command": "ls" }));
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// OpenRouter's `/api/v1/messages` skin sends the `cache_*` token counts as an
/// explicit `null` in every `message_start` (the upstream — here Together — has no
/// caching to report), rather than omitting them as Anthropic does. Regression: a
/// present `null` is not covered by `#[serde(default)]` and crashed the first
/// OpenRouter turn with `invalid type: null, expected u64`. These are the exact
/// bytes captured from `thinkingmachines/inkling`.
#[test]
fn openrouter_null_cache_usage_does_not_crash() {
    let completion = feed(&[
            ("message_start", r#"{"type":"message_start","message":{"id":"gen-1","type":"message","role":"assistant","container":null,"content":[],"model":"thinkingmachines/inkling","stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null,"cache_creation":null,"inference_geo":null,"server_tool_use":null,"service_tier":null,"speed":"standard"},"provider":"Together"}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":17,"output_tokens":64,"cache_creation_input_tokens":null,"cache_read_input_tokens":0}}"#),
        ])
        .unwrap();
    assert_eq!(completion.model, "thinkingmachines/inkling");
    assert_eq!(completion.usage.input_tokens, 0);
    assert_eq!(completion.usage.cache_creation_input_tokens, 0); // null → 0
    assert_eq!(completion.usage.cache_read_input_tokens, 0); // null → 0
    assert_eq!(completion.usage.output_tokens, 64); // message_delta, non-null
}

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
    ])
    .unwrap();
    match completion.assistant {
        Payload::Assistant { calls, .. } => {
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
        ])
        .unwrap();
    match completion.assistant {
        Payload::Assistant {
            thinking,
            text,
            calls,
        } => {
            assert_eq!(
                thinking,
                vec![ContentBlock::Thinking {
                    text: "weighing the options".into(),
                    signature: "SiG==".into(),
                }]
            );
            assert_eq!(text, vec![ContentBlock::Text("here's the answer".into())]);
            assert!(calls.is_empty());
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
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
        ])
        .unwrap();
    match completion.assistant {
        Payload::Assistant { thinking, .. } => assert_eq!(
            thinking,
            vec![ContentBlock::RedactedThinking {
                data: "ENCRYPTED==".into()
            }]
        ),
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// An unknown event type is loud, never silently dropped.
#[test]
fn unknown_event_type_is_loud() {
    let err = feed(&[("surprise", "{}")]).unwrap_err();
    assert!(err.to_string().contains("surprise"), "{err}");
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
        Payload::Assistant { text, .. } => {
            assert_eq!(text, vec![ContentBlock::Text("hi".into())]);
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// An in-stream `error` event surfaces its full body (the path that keeps the
/// useful overload/rate-limit detail the pre-stream non-2xx path loses).
#[test]
fn in_stream_error_surfaces_body() {
    let err = feed(&[(
        "error",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    )])
    .unwrap_err();
    assert!(err.to_string().contains("overloaded_error"), "{err}");
}

use crate::project::ApiBlock;

fn sample_messages() -> Vec<ApiMessage> {
    vec![
        ApiMessage {
            role: crate::project::Role::User,
            content: vec![ApiBlock::Text {
                text: "hello".into(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: crate::project::Role::Assistant,
            content: vec![ApiBlock::Text {
                text: "hi there".into(),
                cache_control: None,
            }],
        },
    ]
}

/// `render_messages_pretty` (the `wire --messages` JSON view) is exactly the
/// projected array — no `system`/`tools`/`model` envelope wrapped around it.
#[test]
fn render_messages_pretty_is_just_the_array() {
    let messages = sample_messages();
    let rendered = render_messages_pretty(&messages).unwrap();
    let value: Value = serde_json::from_str(&rendered).unwrap();
    assert!(value.is_array(), "expected a bare array, got {value}");
    assert_eq!(value, serde_json::to_value(&messages).unwrap());
    assert!(!rendered.contains("\"system\""));
    assert!(!rendered.contains("\"tools\""));
}

/// `render_messages_yaml` unfolds the same array as pseudo-YAML, with no
/// envelope keys either.
#[test]
fn render_messages_yaml_is_just_the_array() {
    let messages = sample_messages();
    let rendered = render_messages_yaml(&messages).unwrap();
    assert!(rendered.contains("hello"));
    assert!(rendered.contains("hi there"));
    assert!(!rendered.contains("system:"));
    assert!(!rendered.contains("tools:"));
}
