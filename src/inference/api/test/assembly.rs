//! Tests for stream event assembly: the output-kind sink, prose, text, split
//! tool-use, the output-cap cut, cache-count merging, and the provider's own
//! cache-diagnostics attribution.

use super::*;

/// The provider's attribution of a cache miss arrives on `message_start` — it is on no
/// other frame of the stream — and rides the completion from there (docs/transport.md).
/// This is the parse the offline report reads: the reason names where the prompt moved,
/// and the count says how much prefix fell after it.
#[test]
fn a_named_cache_miss_rides_the_completion() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"claude-opus-5","usage":{"input_tokens":4,"cache_read_input_tokens":0},"diagnostics":{"cache_miss_reason":{"type":"system_changed","cache_missed_input_tokens":13655}}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        ("message_stop", "{}"),
    ]);

    assert_eq!(
        completion.cache_miss,
        Some(CacheMiss {
            reason: CacheMissReason::SystemChanged,
            cache_missed_input_tokens: Some(13655),
        })
    );
}

/// The three states the provider's `diagnostics` field can be in all read as no
/// attribution (docs/transport.md): the field absent (nothing was sent), the field
/// `null` (a comparison ran and found no divergence, or there was nothing to compare
/// against), and a present object whose `cache_miss_reason` is `null` (the comparison
/// had not finished when the response serialized — it resolves on the next request,
/// which is compared against the fingerprint *this* response stored).
#[test]
fn the_quiet_cache_diagnostics_states_read_as_no_attribution() {
    for diagnostics in [
        "",
        r#","diagnostics":null"#,
        r#","diagnostics":{"cache_miss_reason":null}"#,
    ] {
        let start = format!(
            r#"{{"message":{{"model":"m","usage":{{"input_tokens":1,"output_tokens":1}}{diagnostics}}}}}"#
        );
        let completion = feed(&[
            ("message_start", &start),
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
            ),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_stop", "{}"),
        ]);
        assert_eq!(completion.cache_miss, None, "for {diagnostics}");
    }
}

/// **Every output kind reaches the live sink**, not just prose
/// (docs/transport.md). A sink carrying only text fragments publishes nothing on
/// a turn that thinks and then emits a tool call, which is indistinguishable
/// from a hung harness. The tool call is named at `content_block_start`, before
/// its arguments, because a large input can take seconds to stream.
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
            Streamed::Thinking {
                index: 0,
                text: "weighing ".into()
            },
            Streamed::Thinking {
                index: 0,
                text: "the options".into()
            },
            // The call is named before a single argument byte arrives, and carries the
            // `tool_use` id its result will key on.
            Streamed::ToolStart {
                index: 1,
                id: "toolu_1".into(),
                name: "bash".into()
            },
            Streamed::ToolInput {
                index: 1,
                fragment: "{\"comm".into()
            },
            Streamed::ToolInput {
                index: 1,
                fragment: "and\":\"ls\"}".into()
            },
        ],
        "a thinking-then-tool-call turn must not stream silently"
    );
    // A signature delta carries no live signal — it's replay metadata, not progress.
    assert!(!seen.iter().any(|s| matches!(s, Streamed::Text { .. })));
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
            Streamed::Text {
                index: 0,
                text: "let me ".into()
            },
            Streamed::Text {
                index: 0,
                text: "look".into()
            }
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
        (
            "message_start",
            r#"{"type":"message_start","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":9407}}}"#,
        ),
        ("ping", r#"{"type":"ping"}"#),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"let me look"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash"}}"#,
        ),
        // One JSON object, three arbitrary byte splits.
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"l"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"s\"}"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);

    assert_eq!(completion.model, "claude-haiku-4-5");
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::ToolCalls)
    );
    assert_eq!(completion.usage.input_tokens, 3);
    assert_eq!(completion.usage.cache_read_input_tokens, 9407); // message_start
    assert_eq!(completion.usage.output_tokens, 4); // message_delta, not the placeholder 1

    match completion.assistant {
        Payload::Assistant {
            output: AssistantOutput::Messages(blocks),
            ..
        } => {
            assert_eq!(blocks[0], MessagesOutputBlock::Text("let me look".into()));
            let calls: Vec<_> = blocks
                .iter()
                .filter_map(|block| match block {
                    MessagesOutputBlock::ToolUse(call) => Some(call),
                    _ => None,
                })
                .collect();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "bash");
            assert_eq!(calls[0].id.0, "toolu_9");
            assert_eq!(calls[0].input, serde_json::json!({ "command": "ls" }));
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// A completion that stops at `max_tokens` with a tool call in it is a
/// record-only partial, whether or not the call's arguments parse
/// (docs/transport.md). Live case, 2026-09-02: a `write` cut at the cap arrived
/// as a *valid* JSON object — `contents` truncated, the remainder of the file
/// split across nonsense keys, no `path` — and only the tool's own "missing
/// field `path`" error stopped it. The parse succeeding was the upstream's
/// repair, not the model's call.
#[test]
fn tool_call_cut_at_the_output_cap_is_a_record_only_partial() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"gen-cap","model":"z-ai/glm-5.3-flash","usage":{"input_tokens":10,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"call_1","name":"write","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"contents\": \"scope = \", \"read\": \"x\"}"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":10,"output_tokens":2048}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::OutputCap)
    );
    let why = completion
        .upstream_failure()
        .expect("marked undispatchable");
    assert!(why.contains("output-token cap"), "{why}");
    // The raw call is kept for audit, parsed as it arrived.
    match &completion.assistant {
        Payload::Assistant { output, .. } => {
            let calls = output.tool_calls().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "write");
        }
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }

    // The same cap with no call in flight is a finalized, committable turn:
    // thinking-only or text-only output that ran long is the model's own.
    let plain = feed(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"gen-cap2","model":"z-ai/glm-5.3-flash","usage":{"input_tokens":10,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"long"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":10,"output_tokens":2048}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert!(plain.upstream_failure().is_none());
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
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"gen-1","type":"message","role":"assistant","container":null,"content":[],"model":"thinkingmachines/inkling","stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null,"cache_creation":null,"inference_geo":null,"server_tool_use":null,"service_tier":null,"speed":"standard"},"provider":"Together"}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":17,"output_tokens":64,"cache_creation_input_tokens":null,"cache_read_input_tokens":0}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(completion.model, "thinkingmachines/inkling");
    // Every count comes from `message_delta` here: `message_start` reported zeros
    // and nulls for all four, so the max-merge keeps the delta's numbers.
    assert_eq!(completion.usage.input_tokens, 17);
    assert_eq!(completion.usage.cache_creation_input_tokens, 0); // null → 0
    assert_eq!(completion.usage.cache_read_input_tokens, 0); // null → 0
    assert_eq!(completion.usage.output_tokens, 64); // message_delta, non-null
}

/// OpenRouter fronting a caching upstream reports the real token counts —
/// including `cache_read`/`cache_creation` — only in `message_delta`; its
/// `message_start` usage is all zeros and nulls. Reading `message_start` alone
/// printed `in 0 · cache read 0 · cache write 0` on every OpenRouter turn, so
/// the max-merge is what makes the cache metrics exist off Anthropic.
#[test]
fn openrouter_cache_counts_arrive_in_message_delta() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"gen-2","model":"anthropic/claude-haiku-4.5","usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":12,"output_tokens":26,"cache_creation_input_tokens":165,"cache_read_input_tokens":9407}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(completion.usage.input_tokens, 12);
    assert_eq!(completion.usage.output_tokens, 26);
    assert_eq!(completion.usage.cache_creation_input_tokens, 165);
    assert_eq!(completion.usage.cache_read_input_tokens, 9407);
}

/// OpenRouter's Messages endpoint reports the serving upstream on `message_start`
/// and the billed cost in the final `message_delta`'s `usage` (docs/transport.md).
/// These are the bytes captured live; the cost lands on the receipt and the token
/// counts stay unaffected.
#[test]
fn inline_cost_and_serving_upstream_land_on_the_receipt() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"gen-9","type":"message","role":"assistant","content":[],"model":"z-ai/glm-5.3-flash","stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null,"speed":"standard"},"provider":"InferenceNet"}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":15,"output_tokens":55,"output_tokens_details":{"thinking_tokens":43},"cache_creation_input_tokens":null,"cache_read_input_tokens":0,"is_byok":false,"cost":0.00002855,"cost_details":{"upstream_inference_cost":0.00002855,"upstream_inference_prompt_cost":0.00000105,"upstream_inference_completions_cost":0.0000275}},"context_management":null}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(
        completion.receipt.served_by.as_deref(),
        Some("InferenceNet")
    );
    let cost = completion.receipt.cost.expect("inline cost");
    assert_eq!(cost.total_usd, 0.00002855);
    assert_eq!(cost.prompt_usd, Some(0.00000105));
    assert_eq!(cost.completion_usd, Some(0.0000275));
    assert_eq!(completion.usage.input_tokens, 15);
    assert_eq!(completion.usage.output_tokens, 55);
    assert_eq!(completion.request_usage().receipt, completion.receipt);
}

/// An explicit `null` cost (or none at all, as Anthropic sends) reads as no inline
/// cost, so the request falls back to the generation lookup.
#[test]
fn null_or_missing_cost_reads_as_absent() {
    for usage in [
        r#"{"input_tokens":1,"output_tokens":2,"cost":null,"cost_details":null}"#,
        r#"{"input_tokens":1,"output_tokens":2}"#,
    ] {
        let delta = format!(r#"{{"delta":{{"stop_reason":"end_turn"}},"usage":{usage}}}"#);
        let completion = feed(&[
            (
                "message_start",
                r#"{"message":{"id":"msg-1","model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#,
            ),
            ("message_delta", delta.as_str()),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        assert_eq!(completion.receipt, Receipt::default(), "{usage}");
        assert_eq!(completion.usage.output_tokens, 2);
    }
}

/// The non-streamed keepalive body carries the same `usage.cost` and a top-level
/// `provider`.
#[test]
fn keepalive_body_cost_and_provider_parse() {
    use crate::inference::api::client::KeepaliveResponse;
    let body = r#"{"id":"gen-3","type":"message","model":"m","provider":"InferenceNet","usage":{"input_tokens":15,"output_tokens":0,"cache_read_input_tokens":null,"cost":0.00001,"cost_details":{"upstream_inference_cost":0.00001,"upstream_inference_prompt_cost":0.00001,"upstream_inference_completions_cost":0}}}"#;
    let ping = serde_json::from_str::<KeepaliveResponse>(body)
        .unwrap()
        .into_request_usage();
    assert_eq!(ping.receipt.served_by.as_deref(), Some("InferenceNet"));
    let cost = ping.receipt.cost.unwrap();
    assert_eq!(cost.total_usd, 0.00001);
    assert_eq!(cost.completion_usd, Some(0.0));
    assert_eq!(ping.usage.input_tokens, 15);
    let bare = r#"{"id":"msg-1","model":"m","usage":{"input_tokens":1,"output_tokens":0}}"#;
    let bare = serde_json::from_str::<KeepaliveResponse>(bare)
        .unwrap()
        .into_request_usage();
    assert_eq!(bare.receipt, Receipt::default());
}
