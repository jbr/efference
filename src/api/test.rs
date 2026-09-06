use std::collections::BTreeMap;

use super::assembler::ResponseMeta;
use super::client::record_upstream_failure;
use super::thinking::is_recognized_adaptive_claude;
use super::*;
use crate::log::{FRAME_CAPTURE_CAP, is_captured_header};

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
    let e =
        Endpoint::resolve(Some("openrouter".into()), Some("sk-or-v1-abc".into()), None).unwrap();
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
    let err = Endpoint::resolve(Some("cohere".into()), Some("k".into()), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("anthropic"), "{err}");
    assert!(err.contains("openrouter"), "{err}");
}

/// The Codex backend takes the catalog's limits but not its explicit cache
/// policy: it rejects `prompt_cache_breakpoint`, so the arm reports implicit.
#[tokio::test]
async fn codex_backend_capabilities_report_implicit_caching() {
    let codex = Endpoint::resolve(Some("openai-codex".into()), Some("tok".into()), None)
        .unwrap()
        .fetch_capabilities("gpt-5.6-luna", &[])
        .await
        .unwrap();
    assert_eq!(codex.prompt_cache, PromptCachePolicy::Implicit);
    let platform = Endpoint::resolve(Some("openai".into()), Some("tok".into()), None)
        .unwrap()
        .fetch_capabilities("gpt-5.6-luna", &[])
        .await
        .unwrap();
    assert_eq!(platform.prompt_cache, PromptCachePolicy::Explicit);
    assert_eq!(codex.context_length, platform.context_length);
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
    let caps = Provider::Anthropic.parse_capabilities(body, &[]).unwrap();
    assert_eq!(caps.context_length, 200000);
    assert_eq!(caps.max_output_tokens, Some(64000));
}

/// A Models API response without `max_input_tokens` fails the (required) boot
/// lookup rather than defaulting — the ceiling would otherwise be a guess.
#[test]
fn anthropic_capabilities_missing_context_is_an_error() {
    let err = Provider::Anthropic
        .parse_capabilities(r#"{"id": "m", "display_name": "M"}"#, &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("max_input_tokens"), "{err}");
}
/// The Anthropic metadata path declares no effort levels — not on its single
/// route, and there is no second route — so a configured level passes through
/// to the provider's own validation (docs/transport.md).
#[test]
fn anthropic_declares_no_effort_levels() {
    let body = r#"{
        "id": "claude-haiku-4-5",
        "display_name": "Claude Haiku 4.5",
        "max_input_tokens": 200000,
        "max_tokens": 64000
    }"#;
    let caps = Provider::Anthropic.parse_capabilities(body, &[]).unwrap();
    assert_eq!(caps.supported_efforts, None);
    assert_eq!(caps.default_effort, None);
    assert_eq!(
        Provider::Anthropic
            .effort_levels_url("claude-haiku-4-5")
            .unwrap(),
        None
    );
}

/// The OpenRouter endpoints shape: both ceilings are per serving endpoint, and
/// they aggregate in opposite directions. `context_length` takes the **max** —
/// routing filters to endpoints that fit, so the widest is reachable.
/// `max_completion_tokens` takes the **min** (over the endpoints at or above
/// `OUTPUT_FLOOR`, see the next test) — it is sent as `max_tokens`, and a max
/// there would pin routing to one endpoint. Trimmed from a live
/// `GET /api/v1/models/anthropic/claude-haiku-4.5/endpoints` response.
#[test]
fn openrouter_capabilities_aggregate_ceilings_in_opposite_directions() {
    let body = r#"{
        "data": {
            "id": "anthropic/claude-haiku-4.5",
            "architecture": {"input_modalities": ["text", "image", "file"]},
            "endpoints": [
                {"provider_name": "Google", "context_length": 200000,
                 "max_completion_tokens": 64000, "supports_implicit_caching": false},
                {"provider_name": "Anthropic", "context_length": 195000,
                 "max_completion_tokens": 48000, "supports_implicit_caching": false},
                {"provider_name": "Azure", "context_length": null,
                 "max_completion_tokens": null, "supports_implicit_caching": false}
            ]
        }
    }"#;
    let caps = Provider::OpenRouter.parse_capabilities(body, &[]).unwrap();
    assert_eq!(caps.context_length, 200000);
    assert_eq!(caps.max_output_tokens, Some(48000));
    // No endpoint above lists `supported_parameters` at all → not granted.
    assert!(!caps.supports_reasoning);
}

/// The output-cap min skips endpoints below `OUTPUT_FLOOR` (docs/transport.md):
/// the live case was `z-ai/glm-5.3-flash` with twenty-one endpoints at 128k or
/// more and one at 2048, which held every turn to 2048 output tokens — on a
/// thinking model, a turn cut off before its first tool call. The plain min
/// survives only when nothing clears the floor.
#[test]
fn openrouter_output_cap_skips_endpoints_below_the_floor() {
    let body = |caps: &[Option<u64>]| {
        let endpoints: Vec<String> = caps
            .iter()
            .map(|c| {
                let cap = c.map_or("null".to_string(), |c| c.to_string());
                format!(
                    r#"{{"provider_name": "p", "context_length": 200000,
                        "max_completion_tokens": {cap}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"data": {{"architecture": {{"input_modalities": ["text"]}},
                "endpoints": [{}]}}}}"#,
            endpoints.join(",")
        )
    };
    let cap = |caps: &[Option<u64>]| {
        Provider::OpenRouter
            .parse_capabilities(&body(caps), &[])
            .unwrap()
            .max_output_tokens
    };
    // The outlier is skipped; the min among the rest is sent.
    assert_eq!(
        cap(&[Some(131_072), Some(2_048), Some(128_000), None]),
        Some(128_000)
    );
    // Exactly at the floor counts as clearing it.
    assert_eq!(cap(&[Some(OUTPUT_FLOOR), Some(64_000)]), Some(OUTPUT_FLOOR));
    // Nothing clears the floor: the plain min, so routing stays open.
    assert_eq!(cap(&[Some(4_096), Some(8_192)]), Some(4_096));
    // No endpoint reports a cap at all.
    assert_eq!(cap(&[None, None]), None);
}

/// `supports_reasoning` is min-style like the ceilings: granted only when every
/// endpoint serving the model lists `reasoning` — routing is unpinned, so a
/// `thinking` block must be safe on whichever endpoint the request lands on.
#[test]
fn openrouter_reasoning_capability_requires_every_endpoint() {
    let all = r#"{"data": {"endpoints": [
        {"context_length": 1000000, "supported_parameters": ["reasoning", "tools"]},
        {"context_length": 1000000, "supported_parameters": ["tools", "reasoning"]}
    ]}}"#;
    assert!(
        Provider::OpenRouter
            .parse_capabilities(all, &[])
            .unwrap()
            .supports_reasoning
    );
    let mixed = r#"{"data": {"endpoints": [
        {"context_length": 1000000, "supported_parameters": ["reasoning", "tools"]},
        {"context_length": 1000000, "supported_parameters": ["tools"]}
    ]}}"#;
    assert!(
        !Provider::OpenRouter
            .parse_capabilities(mixed, &[])
            .unwrap()
            .supports_reasoning
    );
}

/// The thinking matrix (docs/transport.md): Claude keeps the Anthropic pairings
/// (adaptive on `auto` for recognized tiers, explicit `disabled` on forced choice
/// and Haiku); foreign models never see `disabled` — adaptive on `auto` when the
/// capability lookup grants `reasoning`, the field omitted otherwise, forced
/// choice included. A mandatory-reasoning endpoint 400s on an explicit disable
/// but thinks and then honors a forced call with the field omitted.
#[test]
fn thinking_matrix_never_disables_foreign_models() {
    use super::thinking::Thinking;
    let auto = ToolChoice::auto();
    let forced = ToolChoice::force("retain");
    let m = Thinking::for_request;
    assert!(matches!(
        m("claude-fable-5", &auto, false),
        Thinking::Adaptive { .. }
    ));
    assert!(matches!(
        m("claude-fable-5", &forced, false),
        Thinking::Disabled
    ));
    assert!(matches!(
        m("claude-haiku-4-5-20251001", &auto, false),
        Thinking::Disabled
    ));
    assert!(matches!(
        m("stealth/ox-alpha", &auto, true),
        Thinking::Adaptive { .. }
    ));
    assert!(matches!(
        m("stealth/ox-alpha", &forced, true),
        Thinking::Omit
    ));
    assert!(matches!(
        m("deepseek/deepseek-chat", &auto, false),
        Thinking::Omit
    ));
    assert!(matches!(
        m("deepseek/deepseek-chat", &forced, false),
        Thinking::Omit
    ));
    assert!(matches!(
        Thinking::for_keepalive("claude-haiku-4-5-20251001"),
        Thinking::Disabled
    ));
    assert!(matches!(
        Thinking::for_keepalive("stealth/ox-alpha"),
        Thinking::Omit
    ));
}

/// An `Omit` never reaches the wire: the request body carries no `thinking` key
/// at all, where a Claude request carries the adaptive block.
#[test]
fn omitted_thinking_is_absent_from_the_request_body() {
    let foreign =
        render_request_pretty("deepseek/deepseek-chat", "", &[], ToolChoice::auto(), &[]).unwrap();
    assert!(!foreign.contains("thinking"), "{foreign}");
    let claude = render_request_pretty("claude-fable-5", "", &[], ToolChoice::auto(), &[]).unwrap();
    assert!(claude.contains("\"thinking\""), "{claude}");
    assert!(claude.contains("adaptive"), "{claude}");
}

/// A model no endpoint serves (or that reports no context length at all) fails
/// the boot lookup with a legible message.
#[test]
fn openrouter_capabilities_no_endpoints_is_an_error() {
    let err = Provider::OpenRouter
        .parse_capabilities(r#"{"data": {"endpoints": []}}"#, &[])
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
/// The OpenRouter model entry's `reasoning` block is the effort metadata.
/// Live shape (2026-09-04): GLM 5.3 flash names its levels and its default;
/// the block's other fields (`mandatory`, `default_enabled`) are ignored.
#[test]
fn openrouter_model_entry_yields_effort_levels() {
    let body = r#"{"data": {"id": "z-ai/glm-5.3-flash", "reasoning": {
        "mandatory": true, "default_enabled": true,
        "supported_efforts": ["max", "high", "low"], "default_effort": "max"
    }}}"#;
    let efforts = Provider::OpenRouter.parse_model_entry(body).unwrap();
    assert_eq!(
        efforts.supported,
        Some(vec!["max".into(), "high".into(), "low".into()])
    );
    assert_eq!(efforts.default.as_deref(), Some("max"));
}

/// An absent block — a non-reasoning model (live: `openai/gpt-4.1-mini`,
/// 2026-09-04) — declares nothing, and an empty list is the same state:
/// neither affirms a level. `default_effort` is read independently, so
/// incoherent metadata (a default with no list) still reaches the boot line
/// rather than disappearing.
#[test]
fn openrouter_model_entry_declaring_nothing_reads_as_none() {
    let absent = Provider::OpenRouter
        .parse_model_entry(r#"{"data": {"id": "openai/gpt-4.1-mini"}}"#)
        .unwrap();
    assert_eq!(absent.supported, None);
    assert_eq!(absent.default, None);

    let empty = Provider::OpenRouter
        .parse_model_entry(
            r#"{"data": {"id": "x",
                "reasoning": {"supported_efforts": [], "default_effort": "high"}}}"#,
        )
        .unwrap();
    assert_eq!(empty.supported, None);
    assert_eq!(empty.default.as_deref(), Some("high"));
}

/// A level outside efference's five-level spelling survives as a string: the
/// metadata's vocabulary is what the boot check tests membership against, not
/// ours — a model that invents `xmax` is validated, not rejected
/// (docs/transport.md).
#[test]
fn openrouter_model_entry_efforts_are_not_vocabulary_checked() {
    let efforts = Provider::OpenRouter
        .parse_model_entry(r#"{"data": {"reasoning": {"supported_efforts": ["xmax", "low-mid"]}}}"#)
        .unwrap();
    assert_eq!(
        efforts.supported,
        Some(vec!["xmax".into(), "low-mid".into()])
    );
    assert_eq!(efforts.default, None);
}

/// OpenRouter's effort-levels route is the singular model entry; Anthropic
/// has no second route and answers `None`. Same author/slug requirement as
/// the ceilings route.
#[test]
fn effort_levels_url_per_provider() {
    assert_eq!(
        Provider::OpenRouter
            .effort_levels_url("z-ai/glm-5.3-flash")
            .unwrap()
            .as_deref(),
        Some("https://openrouter.ai/api/v1/model/z-ai/glm-5.3-flash")
    );
    assert_eq!(
        Provider::Anthropic
            .effort_levels_url("claude-haiku-4-5")
            .unwrap(),
        None
    );
    assert!(
        Provider::OpenRouter
            .effort_levels_url("claude-haiku-4-5")
            .is_err()
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

/// Mirror of `Client::complete`'s read loop (docs/transport.md): an event the
/// assembler refuses stops the feed and is recorded as the stream's failure, which
/// `finish` folds into the completion's `upstream_failure` — the partial is never
/// discarded.
fn feed(events: &[(&str, &str)]) -> Result<Completion, Error> {
    let mut a = Assembler::new();
    for (ty, data) in events {
        if let Err(e) = a.apply(Some(ty), data) {
            a.fail(e.to_string());
            break;
        }
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

/// The chip's all-clear: a 2xx clears the diagnostic, so a recovered session stops
/// reporting a failure that is over (docs/transport.md).
#[test]
fn upstream_failure_clears_on_recovery() {
    let failure = upstream_failure_state();

    record_upstream_failure(&failure, 429);
    assert!(failure.lock().unwrap().is_some());

    super::client::clear_upstream_failure(&failure);
    assert_eq!(*failure.lock().unwrap(), None);

    // A later failure starts a fresh run rather than resuming the cleared count.
    record_upstream_failure(&failure, 429);
    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure {
            status: 429,
            count: 1,
        })
    );
}

/// One abort-retry-finalize cycle on the turn surface: the topic appears when the
/// first attempt aborts, tracks the attempt number, and is cleared by the attempt
/// that finalizes (docs/transport.md).
#[test]
fn retry_topic_tracks_attempts_and_clears_on_finalize() {
    let retry = upstream_retry_state();
    assert_eq!(retry.get(), None);

    for attempt in 1..=3u32 {
        retry.set(crate::agent::stream_abort_retry(
            RetrySurface::Turn,
            attempt,
            std::time::Duration::from_secs(1),
            "upstream SSE stream failed mid-stream: connection reset",
        ));
        let live = retry.get().expect("a retry is in flight");
        assert_eq!(live.surface, RetrySurface::Turn);
        assert_eq!(live.attempt, attempt);
        assert_eq!(live.budget, crate::agent::STREAM_ABORT_ATTEMPTS);
        // Leads with what is happening, not with a status code.
        assert_eq!(
            live.message,
            format!(
                "stream aborted, retrying ({attempt} of {})",
                crate::agent::STREAM_ABORT_ATTEMPTS
            )
        );
        assert!(live.detail.contains("connection reset"));
        assert!(live.next_attempt_at > crate::log::Timestamp::now().0 - 1000);
    }

    retry.clear(RetrySurface::Turn);
    assert_eq!(retry.get(), None);
}

/// Exhaustion is not a state the topic holds: the budget running out leaves it
/// cleared, and the outcome is visible through the notification and the
/// record-only entries instead (docs/transport.md).
#[test]
fn exhaustion_leaves_the_retry_topic_cleared() {
    let retry = upstream_retry_state();
    retry.set(crate::agent::stream_abort_retry(
        RetrySurface::Peek,
        crate::agent::STREAM_ABORT_ATTEMPTS - 1,
        std::time::Duration::from_secs(30),
        "aborted",
    ));
    assert!(retry.get().is_some());

    // The loop's exit on a spent budget: no pause to publish, only the clear.
    assert_eq!(
        crate::agent::stream_abort_backoff(crate::agent::STREAM_ABORT_ATTEMPTS),
        None
    );
    retry.clear(RetrySurface::Peek);
    assert_eq!(retry.get(), None);
}

/// The clear is scoped to its surface, so one loop's all-clear cannot erase the
/// other's live retry — and the two surfaces stay distinguishable, which is the
/// asymmetry that motivated giving the peek its own budget (docs/transport.md).
#[test]
fn a_peek_clear_does_not_erase_a_turn_retry() {
    let retry = upstream_retry_state();
    retry.set(crate::agent::stream_abort_retry(
        RetrySurface::Turn,
        2,
        std::time::Duration::from_secs(2),
        "aborted",
    ));

    retry.clear(RetrySurface::Peek);

    assert_eq!(
        retry.get().map(|r| r.surface),
        Some(RetrySurface::Turn),
        "the turn's retry survives a peek's all-clear"
    );
}

/// Failed attempts queue in arrival order and drain exactly once — the drain is
/// what hands the loop the bodies it turns into durable records
/// (docs/transport.md).
#[test]
fn absorbed_attempts_drain_once_in_order() {
    let queue = upstream_attempt_queue();
    for status in [429u16, 503] {
        push_upstream_attempt(
            &queue,
            UpstreamAttemptFailure {
                status,
                body: format!("{{\"error\":{{\"message\":\"{status} says so\"}}}}"),
            },
        );
    }

    let drained = drain_upstream_attempts(&queue);
    assert_eq!(
        drained.iter().map(|f| f.status).collect::<Vec<_>>(),
        [429, 503]
    );
    assert_eq!(
        drained[0].record_text(),
        "upstream attempt failed (HTTP 429): {\"error\":{\"message\":\"429 says so\"}}"
    );
    assert!(
        drain_upstream_attempts(&queue).is_empty(),
        "a drained queue is empty — the record is durable in the log, not here"
    );
}

/// A queue nobody drains keeps the most recent failures and drops the oldest,
/// so a long outage cannot grow the process without bound (docs/transport.md).
#[test]
fn the_attempt_queue_is_bounded() {
    let queue = upstream_attempt_queue();
    for n in 0..200u16 {
        push_upstream_attempt(
            &queue,
            UpstreamAttemptFailure {
                status: 503,
                body: n.to_string(),
            },
        );
    }
    let drained = drain_upstream_attempts(&queue);
    assert!(drained.len() < 200, "the queue is capped");
    assert_eq!(
        drained.last().expect("non-empty").body,
        "199",
        "the newest failure is the one kept"
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
        ("message_start", r#"{"type":"message_start","message":{"id":"gen-cap","model":"z-ai/glm-5.3-flash","usage":{"input_tokens":10,"output_tokens":0}}}"#),
        ("content_block_start", r#"{"index":0,"content_block":{"type":"tool_use","id":"call_1","name":"write","input":{}}}"#),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"contents\": \"scope = \", \"read\": \"x\"}"}}"#),
        ("content_block_stop", r#"{"index":0}"#),
        ("message_delta", r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":10,"output_tokens":2048}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ])
    .unwrap();
    assert_eq!(completion.stop_reason.as_deref(), Some("max_tokens"));
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
        ("message_start", r#"{"type":"message_start","message":{"id":"gen-cap2","model":"z-ai/glm-5.3-flash","usage":{"input_tokens":10,"output_tokens":0}}}"#),
        ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"long"}}"#),
        ("content_block_stop", r#"{"index":0}"#),
        ("message_delta", r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":10,"output_tokens":2048}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ])
    .unwrap();
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
            ("message_start", r#"{"type":"message_start","message":{"id":"gen-1","type":"message","role":"assistant","container":null,"content":[],"model":"thinkingmachines/inkling","stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null,"cache_creation":null,"inference_geo":null,"server_tool_use":null,"service_tier":null,"speed":"standard"},"provider":"Together"}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":17,"output_tokens":64,"cache_creation_input_tokens":null,"cache_read_input_tokens":0}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();
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
            ("message_start", r#"{"type":"message_start","message":{"id":"gen-2","model":"anthropic/claude-haiku-4.5","usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":12,"output_tokens":26,"cache_creation_input_tokens":165,"cache_read_input_tokens":9407}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();
    assert_eq!(completion.usage.input_tokens, 12);
    assert_eq!(completion.usage.output_tokens, 26);
    assert_eq!(completion.usage.cache_creation_input_tokens, 165);
    assert_eq!(completion.usage.cache_read_input_tokens, 9407);
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
        upstream_failure: None,
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
            MessagesOutputBlock::ToolUse(ToolCall {
                id: CallId("toolu_1".into()),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }),
        ]
    );

    let log = vec![crate::log::LogEntry {
        seq: 0,
        turn: 0,
        at: crate::log::Timestamp(0),
        submission_id: None,
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
    assert_eq!(completion.stop_reason, None);
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
    assert_eq!(completion.stop_reason.as_deref(), Some("end_turn"));
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

// ---- the sticky-routing session key (docs/transport.md) ----

/// The key is derived from the session dir, which is fixed for the session's life,
/// so re-deriving it yields the same bytes — the property the whole mechanism rests
/// on. A key that moved with the projection would re-route exactly when consolidation
/// rewrites the opening messages, which is the failure it exists to prevent.
#[test]
fn session_key_is_stable_across_derivations() {
    let dir = std::path::Path::new(".efference/work");
    assert_eq!(sticky_session_id(dir), sticky_session_id(dir));
}

/// Session ids are unique per `.efference/` dir, not per account: two repos both
/// served `--name work` must not pin each other's routing. The name stays readable
/// in front (the Sessions view is the other thing the key buys) and the path hash
/// separates them.
#[test]
fn session_key_separates_same_name_in_different_dirs() {
    let a = sticky_session_id(std::path::Path::new("alpha/.efference/work"));
    let b = sticky_session_id(std::path::Path::new("beta/.efference/work"));
    assert!(a.starts_with("work-"), "{a}");
    assert!(b.starts_with("work-"), "{b}");
    assert_ne!(a, b);
}

/// `--name` is arbitrary user input, so the 256-char cap has to hold without
/// panicking and without eating the hash. Truncating the tail would drop the
/// disambiguating half — the one thing the cap must not do.
#[test]
fn session_key_caps_by_truncating_the_name_not_the_hash() {
    let long = "x".repeat(400);
    let dir = std::path::PathBuf::from(".efference").join(&long);
    let key = sticky_session_id(&dir);
    assert!(key.len() <= 256, "{}", key.len());
    // the hash survives: the same dir under a shorter name shares no suffix by
    // accident, and this key still ends in one full hash's worth of hex
    let (name, hash) = key.rsplit_once('-').unwrap();
    assert_eq!(hash.len(), 16, "{key}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
    assert!(name.chars().all(|c| c == 'x'), "{key}");
}

/// A multibyte `--name` truncates on a character boundary. A plain byte slice here
/// panics at boot, which is a crash on the operator's first non-ASCII session name.
#[test]
fn session_key_truncates_a_multibyte_name_without_panicking() {
    let long = "é".repeat(400);
    let key = sticky_session_id(&std::path::PathBuf::from(".efference").join(&long));
    assert!(key.len() <= 256, "{}", key.len());
    assert!(key.contains('é'));
}

/// The spotter's key is distinct from the agent's. Its prefix shares nothing with the
/// agent turn, and an explicit key overrides the message hashing that would
/// otherwise separate them for free — so pointing both clients at one model would
/// pin two unrelated prefixes to one route.
#[test]
fn spotter_endpoint_takes_a_distinct_key_that_still_fits_the_cap() {
    let mut e = Endpoint::resolve(None, None, Some("sk-ant-xyz".into())).unwrap();
    let long = "x".repeat(400);
    e.session_id = Some(sticky_session_id(
        &std::path::PathBuf::from(".efference").join(&long),
    ));
    let spot = e.for_spotter();
    assert_ne!(spot.session_id, e.session_id);
    assert!(spot.session_id.unwrap().len() <= 256);
}

/// An endpoint with no key derived — every offline verb — yields no spotter key
/// either, rather than a bare suffix.
#[test]
fn spotter_endpoint_without_a_key_stays_keyless() {
    let e = Endpoint::resolve(None, None, Some("sk-ant-xyz".into())).unwrap();
    assert_eq!(e.session_id, None);
    assert_eq!(e.for_spotter().session_id, None);
}

/// Only providers that read a sticky key get the header. Anthropic has no
/// sticky-routing concept, so gating here is what keeps the design off the
/// assumption that an unrecognized header is ignored rather than rejected.
#[test]
fn only_openrouter_declares_a_session_header() {
    assert_eq!(Provider::Anthropic.session_header(), None);
    assert_eq!(Provider::OpenRouter.session_header(), Some("x-session-id"));
}
/// The session-level 0–100 integer temperature maps to the provider's
/// native float range. 50 is mid-scale and lands at 1.0 on both providers
/// (matching both defaults), 0 is the floor, and 100 is the ceiling.
#[test]
fn map_temperature_maps_the_integer_to_provider_native_range() {
    // Anthropic: 0.0..1.0
    let a = Provider::Anthropic;
    assert_eq!(a.map_temperature(0), 0.0);
    assert_eq!(a.map_temperature(50), 0.5);
    assert_eq!(a.map_temperature(100), 1.0);

    // OpenRouter: 0.0..2.0
    let o = Provider::OpenRouter;
    assert_eq!(o.map_temperature(0), 0.0);
    assert_eq!(o.map_temperature(50), 1.0);
    assert_eq!(o.map_temperature(100), 2.0);
}
/// When `temperature` is `None`, the serialized `Request` omits the field
/// entirely — the JSON key is absent, not null. Omitting optional fields
/// vs. explicitly sending defaults produces different cache keys, so this
/// is the cache-safe path for sessions that don't set `--temperature`.
#[test]
fn request_omits_temperature_when_none() {
    let request = Request {
        model: "test",
        max_tokens: 1024,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Disabled,
        output_config: None,
        temperature: None,
        upstream: None,
        prompt_cache_key: None,
        messages: &[],
        stream: false,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        !json.contains("temperature"),
        "temperature field absent when None"
    );
}

/// When `temperature` is `Some(f)`, the serialized `Request` includes the
/// float value. This is the cache-affecting path — the value is part of the
/// request body hash.
#[test]
fn request_includes_temperature_when_set() {
    let request = Request {
        model: "test",
        max_tokens: 1024,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Disabled,
        output_config: None,
        temperature: Some(0.7),
        upstream: None,
        prompt_cache_key: None,
        messages: &[],
        stream: false,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"temperature\":0.7"),
        "temperature: 0.7 present"
    );
}

/// **The whole request envelope, pinned to its exact bytes.**
///
/// The `messages` pins in `src/project/test.rs` cover the projection's own
/// output; the provider matches the **whole body**, and the gap between those
/// two facts has already cost a silent cache break — a tools-array mutation
/// moved the prefix without failing either projection pin
/// (docs/log-and-projection.md). This pin closes that gap: every field ahead of
/// the tail, in serialization order, with `messages` present only far enough to
/// fix its position relative to the rest.
///
/// If this fails, the change under test moves the cached prefix in production.
/// That is the point — update the expectation deliberately, having decided the
/// move is worth a cold cache for every live session, rather than reflexively.
///
/// Adding a field is caught too: a new key lands somewhere in these bytes.
#[test]
fn request_envelope_matches_expected_wire_bytes() {
    use crate::project::{ApiBlock, ApiMessage, CacheControl, Role};

    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run a command",
        "input_schema": {"type": "object"},
    })];
    let messages = [ApiMessage {
        role: Role::User,
        content: vec![ApiBlock::Text {
            text: "hello".into(),
            cache_control: Some(CacheControl::Ephemeral),
        }],
    }];
    let request = Request {
        model: "claude-opus-5",
        max_tokens: 4096,
        system: "you are a harness",
        tools: &tools,
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Disabled,
        output_config: None,
        temperature: Some(0.7),
        upstream: None,
        prompt_cache_key: None,
        messages: &messages,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"claude-opus-5","max_tokens":4096,"system":"you are a harness","#,
        r#""tools":[{"description":"run a command","input_schema":{"type":"object"},"name":"bash"}],"#,
        r#""tool_choice":{"type":"auto","disable_parallel_tool_use":true},"#,
        r#""thinking":{"type":"disabled"},"temperature":0.7,"#,
        r#""messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}],"#,
        r#""stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// The same envelope with an effort set — the other side of the pin above, which
/// holds because `output_config` is absent when the session sets none. This fixes
/// the with-effort bytes: the object is `{"effort":"<level>"}` nested under
/// `output_config`, and it sits **between `thinking` and `temperature`**, so the
/// two reasoning knobs are adjacent (docs/transport.md).
///
/// Same rule as the pin above: a session's effort is boot-time and frozen, so
/// these bytes never change under a live cache — but if this test fails, the
/// change moves the prefix of every session that sets an effort, so update it
/// deliberately.
#[test]
fn with_effort_envelope_matches_expected_wire_bytes() {
    use crate::project::{ApiBlock, ApiMessage, CacheControl, Role};

    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run a command",
        "input_schema": {"type": "object"},
    })];
    let messages = [ApiMessage {
        role: Role::User,
        content: vec![ApiBlock::Text {
            text: "hello".into(),
            cache_control: Some(CacheControl::Ephemeral),
        }],
    }];
    let request = Request {
        model: "claude-opus-5",
        max_tokens: 4096,
        system: "you are a harness",
        tools: &tools,
        tool_choice: ToolChoice::auto(),
        thinking: THINKING_ADAPTIVE,
        output_config: Some(OutputConfig {
            effort: "xhigh".to_string(),
        }),
        temperature: Some(0.7),
        upstream: None,
        prompt_cache_key: None,
        messages: &messages,
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"claude-opus-5","max_tokens":4096,"system":"you are a harness","#,
        r#""tools":[{"description":"run a command","input_schema":{"type":"object"},"name":"bash"}],"#,
        r#""tool_choice":{"type":"auto","disable_parallel_tool_use":true},"#,
        r#""thinking":{"type":"adaptive","display":"summarized"},"#,
        r#""output_config":{"effort":"xhigh"},"temperature":0.7,"#,
        r#""messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}],"#,
        r#""stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// The levels that are legal only with thinking on: exactly the two spellings
/// in [`EFFORTS_REQUIRING_THINKING`], known because the provider answers the
/// pairing with a 400. Any other spelling has no known coupling.
#[test]
fn the_two_spellings_requiring_thinking_are_those_two() {
    assert_eq!(
        super::thinking::EFFORTS_REQUIRING_THINKING,
        ["xhigh", "max"]
    );
}

/// The one coupling between the level axis and the type axis, checked at boot:
/// `xhigh`/`max` with thinking off is a 400 from the provider, so the session is
/// refused at startup with a message naming both halves (docs/transport.md).
/// Every other pairing — including a session that sets no effort at all, and a
/// level with no known coupling — passes.
#[test]
fn thinking_off_rejects_the_two_levels_that_need_thinking() {
    let why = crate::api::check_effort_thinking(Some("max"), true).unwrap_err();
    assert!(why.contains("max"), "names the level: {why}");
    assert!(
        why.contains("EFFERENCE_THINKING"),
        "names one route to thinking-off: {why}"
    );
    assert!(crate::api::check_effort_thinking(Some("xhigh"), true).is_err());
    assert!(crate::api::check_effort_thinking(Some("high"), true).is_ok());
    // A spelling outside the known vocabulary rides through — its coupling,
    // if any, is the provider's to answer, not a local list's to guess.
    assert!(crate::api::check_effort_thinking(Some("xmax"), true).is_ok());
    assert!(crate::api::check_effort_thinking(None, true).is_ok());
    for effort in ["low", "medium", "high"] {
        assert!(crate::api::check_effort_thinking(Some(effort), false).is_ok());
    }
}

/// The boot membership check (docs/transport.md): a configured level rides the
/// wire only when the metadata names it; the declared default, when present,
/// points at the unset path in the rejection.
#[test]
fn check_effort_rejects_unnamed_levels() {
    let caps = caps_with_effort_levels(Some(&["max", "high", "low"]), Some("max"));
    assert!(caps.check_effort("high", "z-ai/glm-5.3-flash").is_ok());

    let why = caps
        .check_effort("medium", "z-ai/glm-5.3-flash")
        .unwrap_err();
    assert!(why.contains("medium"), "names the level: {why}");
    assert!(
        why.contains("max, high, low"),
        "names the levels as the metadata spells them: {why}"
    );
    assert!(why.contains("silently"), "names the mapping hazard: {why}");
    assert!(
        why.contains("provider default: max"),
        "names the unset path: {why}"
    );

    // No declared default → the message doesn't invent one.
    let caps = caps_with_effort_levels(Some(&["xhigh", "high"]), None);
    let why = caps.check_effort("low", "m").unwrap_err();
    assert!(!why.contains("default"), "{why}");
}

/// A model that declares no levels — absent block, absent field, or an empty
/// list, all normalized to `None` — refuses every configured level: nothing
/// was affirmed, so nothing is sent (docs/transport.md).
#[test]
fn check_effort_refuses_any_configured_level_without_a_declared_list() {
    let caps = caps_with_effort_levels(None, Some("high"));
    let why = caps
        .check_effort("high", "openai/gpt-4.1-mini")
        .unwrap_err();
    assert!(why.contains("declares no supported effort levels"), "{why}");
    assert!(why.contains("openai/gpt-4.1-mini"), "{why}");
}

/// Test fixture: a [`ModelCapabilities`] with the effort metadata set and the
/// other fields at neutral values.
fn caps_with_effort_levels(
    levels: Option<&[&str]>,
    default: Option<&str>,
) -> crate::api::ModelCapabilities {
    crate::api::ModelCapabilities {
        context_length: 200_000,
        max_output_tokens: None,
        supports_reasoning: true,
        supports_image_input: false,
        prompt_cache: PromptCachePolicy::Explicit,
        supported_efforts: levels.map(|l| l.iter().map(|s| s.to_string()).collect()),
        default_effort: default.map(String::from),
    }
}

/// The boot check's `thinking_off` input covers *both* routes to disabled, not
/// just the env override: a Claude model outside the adaptive rule whose
/// capability lookup denies reasoning (Haiku is the concrete case) falls through
/// to explicit `disabled` on every auto turn, so `--effort max` there must refuse
/// at boot rather than 400 on the first turn. Assumes `EFFERENCE_THINKING` is
/// unset in the test environment, like every other test of the thinking matrix.
#[test]
fn the_capability_route_to_thinking_off_is_covered() {
    use crate::api::auto_turns_disable_thinking as off;
    // Haiku: Claude family, not recognized-adaptive, no reasoning grant → disabled.
    assert!(off("claude-haiku-4-5", false));
    assert!(off("anthropic/claude-haiku-4.5", false));
    // Adaptive Claude thinks; a reasoning grant rescues an unrecognized Claude.
    assert!(!off("claude-opus-4-8", false));
    assert!(!off("claude-haiku-4-5", true));
    // A foreign model without reasoning gets `Omit`, not `disabled` — the
    // endpoint decides, so the boot check stays out of it.
    assert!(!off("deepseek/deepseek-v4-flash", false));
}

/// The pinned envelope above is what an **Anthropic** session sends, and adding
/// `prompt_cache_key` did not move a byte of it — the field is `None` there and
/// skips serialization entirely. This pins the other half: an OpenRouter session
/// carries the key, and it sits between `provider` and `messages`, so the two
/// provider-conditional fields are adjacent rather than straddling the long
/// `messages` block. Both are per-session constants, so neither can toggle
/// mid-session and move a live cached prefix (docs/transport.md).
#[test]
fn openrouter_envelope_carries_the_prompt_cache_key() {
    let pin = Upstream::pinned(vec!["google-ai-studio".into()]).expect("a non-empty order");
    let request = Request {
        model: "google/gemini-3.7-flash",
        max_tokens: 1,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Omit,
        output_config: None,
        temperature: None,
        upstream: Some(&pin),
        prompt_cache_key: Some("work-0123456789abcdef"),
        messages: &[],
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"google/gemini-3.7-flash","max_tokens":1,"tools":[],"#,
        r#""tool_choice":{"type":"auto","disable_parallel_tool_use":true},"#,
        r#""provider":{"order":["google-ai-studio"],"allow_fallbacks":false},"#,
        r#""prompt_cache_key":"work-0123456789abcdef","#,
        r#""messages":[],"stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// Which providers read the body field, and which read the header. OpenRouter
/// takes both (the header wins its own precedence order; the body copy is what an
/// OpenAI-dialect upstream reads for its own cache). Anthropic takes neither —
/// there is one upstream to route to, and an unrecognized top-level key is a 400
/// there, so this cannot be sent unconditionally (docs/transport.md).
#[test]
fn only_openrouter_carries_the_sticky_key() {
    assert!(!Provider::Anthropic.prompt_cache_key_field());
    assert!(Provider::OpenRouter.prompt_cache_key_field());
    assert_eq!(Provider::Anthropic.session_header(), None);
    assert_eq!(Provider::OpenRouter.session_header(), Some("x-session-id"));
}

/// The `--upstream` pin narrows which endpoints the ceilings aggregate over
/// (docs/transport.md). Unpinned, `context_length` is the max across every
/// serving endpoint; pinned, it is the max across the pinned ones — the session
/// will never reach the others, so their numbers are someone else's. A pin naming
/// no listed endpoint falls back to the whole set rather than emptying it, so the
/// failure surfaces as the request OpenRouter rejects rather than as a boot error
/// about a missing `context_length`.
#[test]
fn upstream_pin_narrows_the_capability_aggregation() {
    let body = r#"{
        "data": {
            "architecture": {"input_modalities": ["text", "image"]},
            "endpoints": [
                {"tag": "google-ai-studio", "context_length": 200000,
                 "max_completion_tokens": 64000,
                 "pricing": {"input_cache_read": "$0.1/M tokens"}},
                {"tag": "google-vertex/global", "context_length": 1000000,
                 "max_completion_tokens": 8000, "pricing": {}}
            ]
        }
    }"#;
    let caps = |upstream: &[String]| {
        Provider::OpenRouter
            .parse_capabilities(body, upstream)
            .unwrap()
    };
    let unpinned = caps(&[]);
    assert_eq!(unpinned.context_length, 1_000_000);
    // 8_000 is below `OUTPUT_FLOOR`, so the unpinned min skips it.
    assert_eq!(unpinned.max_output_tokens, Some(64_000));

    let pinned = caps(&["google-ai-studio".to_string()]);
    assert_eq!(pinned.context_length, 200_000);
    assert_eq!(pinned.max_output_tokens, Some(64_000));

    let unknown = caps(&["not-an-endpoint".to_string()]);
    assert_eq!(unknown.context_length, 1_000_000);
}

/// The keepalive gate (docs/transport.md): a `max_tokens: 0` ping is only free
/// where there is a cache entry to read, so it is armed off the metadata. Two
/// sufficient signals — an upstream that caches implicitly, or one that prices
/// cache reads (how the explicit-`cache_control` upstreams report having a
/// cache). Neither, and the keepalive stays off rather than re-billing the whole
/// prefix at the input rate every interval.
#[test]
fn prompt_caching_is_read_off_the_endpoint_metadata() {
    let caps = |endpoints: &str| {
        Provider::OpenRouter
            .parse_capabilities(&format!(r#"{{"data": {{"endpoints": {endpoints}}}}}"#), &[])
            .unwrap()
    };
    assert_eq!(
        caps(r#"[{"context_length": 1, "pricing": {"input_cache_read": "$0.1/M tokens"}}]"#)
            .prompt_cache,
        PromptCachePolicy::Explicit,
    );
    assert_eq!(
        caps(r#"[{"context_length": 1, "supports_implicit_caching": true}]"#).prompt_cache,
        PromptCachePolicy::Implicit,
    );
    assert_eq!(
        caps(
            r#"[{"context_length": 1, "supports_implicit_caching": true,
                  "pricing": {"input_cache_read": "$0.1/M tokens"}}]"#
        )
        .prompt_cache,
        PromptCachePolicy::Mixed,
    );
    assert_eq!(
        caps(r#"[{"context_length": 1, "pricing": {"prompt": "$1/M tokens"}}]"#).prompt_cache,
        PromptCachePolicy::None,
    );
    // Anthropic's own API is the explicit-cache case and never has to be asked.
    assert_eq!(
        Provider::Anthropic
            .parse_capabilities(r#"{"max_input_tokens": 200000}"#, &[])
            .unwrap()
            .prompt_cache,
        PromptCachePolicy::Explicit,
    );
}

/// OpenAI's Models endpoint does not carry the fields boot consumes, so the
/// current family is an exact compiled profile rather than a partial remote
/// response. The alias is listed separately and unknown IDs do not inherit a
/// family-shaped guess.
#[test]
fn openai_gpt_5_6_capability_profiles_are_exact() {
    for model in ["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
        let caps = super::provider::openai_model_capabilities(model).unwrap();
        assert_eq!(caps.context_length, 1_050_000, "{model}");
        assert_eq!(caps.max_output_tokens, Some(128_000), "{model}");
        assert!(caps.supports_reasoning, "{model}");
        assert!(caps.supports_image_input, "{model}");
        assert_eq!(caps.prompt_cache, PromptCachePolicy::Explicit, "{model}");
        assert_eq!(caps.default_effort.as_deref(), Some("medium"), "{model}");
        assert_eq!(
            caps.supported_efforts.as_deref(),
            Some(&["none", "low", "medium", "high", "xhigh", "max"].map(String::from)[..]),
            "{model}",
        );
    }

    let error = super::provider::openai_model_capabilities("gpt-5.7-luna")
        .unwrap_err()
        .to_string();
    assert!(error.contains("no capability profile"), "{error}");
    assert!(error.contains("gpt-5.7-luna"), "{error}");
}

/// Image input, per provider (docs/tools.md). OpenRouter states it in
/// `architecture.input_modalities`, and its absence is read as text-only — the
/// fail-closed direction for a field whose job is to stop bytes a model cannot
/// read. Anthropic states it in `capabilities.image_input.supported`, and its
/// **absence** is read as supported: the Messages API has taken image blocks on
/// every Claude model since it shipped, so a metadata response that stops
/// carrying the field must not disable a working tool.
#[test]
fn image_input_is_read_per_provider_with_opposite_defaults() {
    let openrouter = |modalities: &str| {
        Provider::OpenRouter
            .parse_capabilities(
                &format!(
                    r#"{{"data": {{"architecture": {{"input_modalities": {modalities}}},
                       "endpoints": [{{"context_length": 1}}]}}}}"#
                ),
                &[],
            )
            .unwrap()
            .supports_image_input
    };
    assert!(openrouter(r#"["text", "image"]"#));
    assert!(!openrouter(r#"["text"]"#));
    assert!(
        !Provider::OpenRouter
            .parse_capabilities(r#"{"data": {"endpoints": [{"context_length": 1}]}}"#, &[])
            .unwrap()
            .supports_image_input,
        "no architecture at all reads as text-only"
    );

    let anthropic = |body: &str| {
        Provider::Anthropic
            .parse_capabilities(body, &[])
            .unwrap()
            .supports_image_input
    };
    assert!(anthropic(
        r#"{"max_input_tokens": 1, "capabilities": {"image_input": {"supported": true}}}"#
    ));
    assert!(!anthropic(
        r#"{"max_input_tokens": 1, "capabilities": {"image_input": {"supported": false}}}"#
    ));
    assert!(
        anthropic(r#"{"max_input_tokens": 1}"#),
        "an absent field must not disable a capability that works"
    );
}

/// The envelope's two omit-when-unset fields drop out **entirely** rather than
/// serializing null or a default: an absent key and a key with a default value
/// are different bytes, so this is the shape a session that sets neither
/// `--temperature` nor a system prompt actually sends.
#[test]
fn request_envelope_omits_unset_fields_entirely() {
    let request = Request {
        model: "m",
        max_tokens: 1,
        system: "",
        tools: &[],
        tool_choice: ToolChoice::auto(),
        thinking: Thinking::Omit,
        output_config: None,
        temperature: None,
        upstream: None,
        prompt_cache_key: None,
        messages: &[],
        stream: true,
    };
    let expected = concat!(
        r#"{"model":"m","max_tokens":1,"tools":[],"#,
        r#""tool_choice":{"type":"auto","disable_parallel_tool_use":true},"#,
        r#""messages":[],"stream":true}"#,
    );
    assert_eq!(serde_json::to_string(&request).unwrap(), expected);
}

/// Tool definitions serialize with **sorted keys**, whatever order they were
/// declared in. This is what makes `tools[]` byte-stable across processes:
/// `Value`'s map sorts, where a `HashMap` would randomize per process and move
/// the prefix on every restart (docs/log-and-projection.md).
#[test]
fn tool_definition_keys_serialize_sorted() {
    let declared = serde_json::json!({"zeta": 1, "alpha": 2, "input_schema": 3});
    assert_eq!(
        serde_json::to_string(&declared).unwrap(),
        r#"{"alpha":2,"input_schema":3,"zeta":1}"#,
    );
}

/// **Two tool calls in one response stay distinguishable on the live sink.** Blocks
/// stream sequentially, so an unscoped sink cannot tell the second call's input from the
/// first's continuing: a client holding one slot per kind resets it when the second
/// `ToolStart` arrives and the first call's arriving arguments vanish from the display.
/// Carrying `index` — and the `tool_use` id the eventual result keys on — is what lets a
/// client bind each fragment to the call it belongs to (docs/control-api.md).
#[test]
fn parallel_tool_calls_are_scoped_to_their_blocks() {
    let seen = live(&[
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_a","name":"read","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"paths\""}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_b","name":"bash","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\""}}"#,
        ),
        ("content_block_stop", r#"{"index":1}"#),
    ]);
    assert_eq!(
        seen,
        vec![
            Streamed::ToolStart {
                index: 0,
                id: "toolu_a".into(),
                name: "read".into()
            },
            Streamed::ToolInput {
                index: 0,
                fragment: "{\"paths\"".into()
            },
            Streamed::ToolStart {
                index: 1,
                id: "toolu_b".into(),
                name: "bash".into()
            },
            Streamed::ToolInput {
                index: 1,
                fragment: "{\"command\"".into()
            },
        ],
        "each call's fragments must name the block they belong to"
    );
}

/// A pinned upstream serializes as OpenRouter's `provider` object, and an
/// unpinned one leaves the wire **byte-identical** to before the field existed.
/// The second half is the cache-safety claim: adding a request field that
/// serializes unconditionally would move every session's prefix
/// (docs/log-and-projection.md), so `None` must skip entirely rather than emit
/// `null`. The rename is pinned too — the type is `Upstream` because `Provider`
/// already means anthropic-vs-openrouter, and only the wire says `provider`.
#[test]
fn a_pinned_upstream_renders_as_the_provider_object() {
    let pin = Upstream::pinned(vec!["google-ai-studio".into()]).expect("non-empty order");
    let json = serde_json::to_string(&pin).expect("serializes");
    assert_eq!(
        json,
        r#"{"order":["google-ai-studio"],"allow_fallbacks":false}"#
    );

    // An empty order is not an empty object on the wire — it is no pin at all.
    assert_eq!(Upstream::pinned(Vec::new()), None);

    let tools: [Value; 0] = [];
    let messages = [ApiMessage {
        role: crate::project::Role::User,
        content: vec![crate::project::ApiBlock::Text {
            text: "hi".into(),
            cache_control: None,
        }],
    }];
    let render = |upstream| {
        serde_json::to_string(&Request {
            model: "m",
            max_tokens: 1,
            system: "",
            tools: &tools,
            tool_choice: ToolChoice::auto(),
            thinking: Thinking::Omit,
            output_config: None,
            temperature: None,
            upstream,
            prompt_cache_key: None,
            messages: &messages,
            stream: true,
        })
        .expect("serializes")
    };
    assert!(!render(None).contains("provider"));
    assert!(render(Some(&pin)).contains(r#""provider":{"order":["google-ai-studio""#));
}

/// A stream that **finalizes with nothing** is marked retryable, the complement
/// of one that dies before finalizing (docs/transport.md): proper termination,
/// zero content blocks, zero usage. Unmarked it commits as the model choosing
/// silence, so the loop has
/// nothing to dispatch and idles with the task unfinished. Marked, the turn loop
/// and the fork peek each retry it on the budget they already have.
#[test]
fn a_stream_that_finalizes_empty_is_marked_retryable() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"google/gemini-3.7-flash","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        ),
        ("message_stop", "{}"),
    ])
    .unwrap();
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("no content"), "{why}");
}

/// A stream that finalizes carrying a tool input the fragments do not spell as
/// valid JSON is marked retryable too (docs/transport.md), not returned as an
/// error — a foreign model can send `message_stop` after `input_json_delta`
/// fragments that end mid-string. As an error it leaves the
/// abort-retry budget unspent and puts the serde message where the model could
/// read it; marked, it is a record-only partial the loop retries, and the raw
/// fragment survives as a string — the entry is never projected or dispatched,
/// and may be the only copy of it.
#[test]
fn a_finalized_stream_with_unparseable_tool_input_is_marked_retryable() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"z-ai/glm-5.3","usage":{"input_tokens":9,"output_tokens":1}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"echo cut"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ])
    .expect("a malformed tool input is a marked completion, not an error");
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("unparseable tool input"), "{why}");
    match &completion.assistant {
        Payload::Assistant { output, .. } => assert_eq!(
            output.tool_calls().unwrap()[0].input,
            serde_json::Value::String(r#"{"command": "echo cut"#.to_string()),
            "the raw fragment is kept verbatim"
        ),
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// The empty stream is the one path that keeps evidence (docs/transport.md).
/// Nothing else survives it — the generation id it allocates 404s at OpenRouter
/// on both `GET /generation` and `GET /generation/content` — so the record-only
/// entry carries the response status, the allowlisted response headers (the
/// edge's own `x-generation-id`), the `message_start` body as received, and the
/// frames that arrived.
#[test]
fn an_empty_stream_records_status_headers_and_frames() {
    let mut a = Assembler::new();
    a.observed_response(ResponseMeta {
        status: 200,
        headers: BTreeMap::from([
            ("x-generation-id".to_string(), "gen-abc123".to_string()),
            ("content-type".to_string(), "text/event-stream".to_string()),
        ]),
    });
    let message_start = r#"{"message":{"id":"gen-abc123","model":"m","provider":"Google AI Studio","usage":{"input_tokens":0,"output_tokens":0}}}"#;
    a.apply(Some("message_start"), message_start).unwrap();
    a.apply(Some("data"), "[DONE]").unwrap();
    let completion = a.finish().unwrap();

    let Payload::Assistant {
        upstream_failure: Some(failure),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.headers.get("x-generation-id").map(String::as_str),
        Some("gen-abc123")
    );
    // Kept whole, so a field the assembler does not model — OpenRouter's
    // `provider`, naming the serving leg — reaches the log anyway.
    assert_eq!(stream.message_start.as_deref(), Some(message_start));
    assert!(stream.frames.contains("event: message_start"), "{stream:?}");
    assert!(stream.frames.contains("data: [DONE]"), "{stream:?}");
    assert!(!stream.frames_truncated);
}

/// A response header outside [`CAPTURED_HEADERS`] never enters the record. The
/// allowlist is the guard against an edge attaching an account-scoped header the
/// log then starts keeping (docs/env.md).
#[test]
fn captured_headers_are_an_allowlist() {
    assert!(is_captured_header("X-Generation-Id"));
    assert!(is_captured_header("content-type"));
    assert!(!is_captured_header("set-cookie"));
    assert!(!is_captured_header("authorization"));
}

/// The capture is bounded. An empty stream is a few hundred bytes by definition,
/// so the cap only ever fires on a stream this record is not for: the head is
/// kept, the rest dropped, and the entry says which.
#[test]
fn frame_capture_stops_at_the_cap() {
    let mut a = Assembler::new();
    let filler = "x".repeat(512);
    for _ in 0..64 {
        a.apply(Some("ping"), &format!("{{\"pad\":\"{filler}\"}}"))
            .unwrap();
    }
    a.apply(
        Some("message_start"),
        r#"{"message":{"model":"m","usage":{"input_tokens":0,"output_tokens":0}}}"#,
    )
    .unwrap();
    a.apply(Some("data"), "[DONE]").unwrap();
    let completion = a.finish().unwrap();

    let Payload::Assistant {
        upstream_failure: Some(failure),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert!(stream.frames_truncated);
    assert!(
        stream.frames.len() <= FRAME_CAPTURE_CAP,
        "{}",
        stream.frames.len()
    );
}

/// Every other failure path stays the bare prose it was: a stream that dies
/// before finalizing has its partial, which the diagnostics do not add to, and
/// the entry serializes to the same bytes older logs wrote.
#[test]
fn a_death_before_finalizing_carries_no_diagnostics() {
    let completion = feed(&[(
        "message_start",
        r#"{"message":{"model":"m","usage":{"input_tokens":3,"output_tokens":0}}}"#,
    )])
    .unwrap();
    let Payload::Assistant {
        upstream_failure: Some(failure),
        ..
    } = &completion.assistant
    else {
        panic!("an unfinalized stream is a record-only entry")
    };
    assert!(failure.stream.is_none());
    assert_eq!(
        serde_json::to_value(failure).unwrap(),
        serde_json::json!("stream ended before finalizing (no message_stop)")
    );
}

/// …but **only** with zero usage to corroborate. A turn billed for output that
/// nonetheless carries no blocks is the model's own answer, not a gateway
/// swallowing one; marking it would retry a real response into exhaustion.
#[test]
fn an_empty_turn_that_was_billed_is_left_alone() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"m","usage":{"input_tokens":12,"output_tokens":7}}}"#,
        ),
        ("message_stop", "{}"),
    ])
    .unwrap();
    assert!(
        completion.upstream_failure().is_none(),
        "a billed turn is not an empty stream"
    );
}

/// **The duration ceiling** (docs/transport.md): a stream that opens, trickles,
/// and then never finalizes is given up on rather than blocking its turn forever,
/// and the completion comes back marked exactly the way a mid-stream abort is —
/// record-only, retryable, with the reason on it. The server here holds the
/// connection open with an unterminated SSE body, which is the shape nothing
/// bounded before this: the retry policy applies only while *opening* a response.
///
/// One read loop serves both stream surfaces, so a turn and a fork peek are
/// bounded by the same knob.
#[tokio::test]
async fn a_stream_open_past_the_ceiling_is_marked_retryable() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // The server holds the connection open until the test says it is done, so the
    // *client's* ceiling is always what ends the read. A server that closed on its
    // own timer would race it, and an EOF already waiting on the socket wins over an
    // elapsed deadline — the assertion would then be about the wrong failure.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = std::sync::Arc::clone(&done);
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf);
        let event = "event: message_start\ndata: {\"message\":{\"model\":\"m\",\
                     \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 transfer-encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n",
                event.len()
            )
            .as_bytes(),
        )
        .unwrap();
        sock.flush().unwrap();
        // Never send the terminating chunk: the response is open and unfinalized,
        // which is the shape nothing bounded before this.
        sock.set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        while !held.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = sock.read(&mut buf);
        }
    });

    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: format!("http://{addr}/v1/messages"),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
    };
    let client = Client::new(endpoint, "m", "sys", None)
        .with_stream_ceiling(std::time::Duration::from_millis(200));
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the ceiling is not an open failure — it assembles a partial");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("ceiling"), "{why}");
    assert!(why.contains("stream-ceiling"), "names the knob: {why}");
    server.join().unwrap();
}

/// End to end over a socket: the diagnostics come off the *response*, so the
/// hook is in `complete` — the status handler cannot serve this path, an empty
/// stream being a 200 it early-returns on. The server here answers exactly as a
/// gateway fronting a slow upstream does: 200, an `x-generation-id`, a
/// `message_start` with all-zero usage, `[DONE]`, nothing else.
#[tokio::test]
async fn an_empty_stream_over_the_wire_captures_the_response_headers() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf);
        let body = "event: message_start\ndata: {\"message\":{\"id\":\"gen-abc123\",\
                    \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                    event: data\ndata: [DONE]\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 x-generation-id: gen-abc123\r\nset-cookie: session=nope\r\n\
                 content-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        sock.flush().unwrap();
    });

    let endpoint = Endpoint {
        provider: Provider::OpenRouter,
        url: format!("http://{addr}/api/v1/messages"),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
    };
    let client = Client::new(endpoint, "m", "sys", None);
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an empty stream assembles a record-only entry, not an error");
    server.join().unwrap();

    let Payload::Assistant {
        upstream_failure: Some(failure),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.headers.get("x-generation-id").map(String::as_str),
        Some("gen-abc123")
    );
    // Off the allowlist, so it never reaches the log (docs/env.md).
    assert!(!stream.headers.contains_key("set-cookie"));
    assert!(stream.frames.contains("data: [DONE]"), "{stream:?}");
}

/// The ceiling is a `[transport]` knob with a generous default, not a hardcoded
/// number: the right value is workload-dependent (docs/transport.md).
#[test]
fn the_stream_ceiling_defaults_generously_and_is_operator_settable() {
    assert_eq!(
        crate::config::TransportLimits::default().stream_ceiling,
        DEFAULT_STREAM_CEILING
    );
    // Comfortably past the widest model call in this repo's corpus (363s).
    assert!(DEFAULT_STREAM_CEILING >= std::time::Duration::from_secs(3600));
}

/// Which envelopes carry the effort, over a real socket: the turn (and the fork
/// peek, which is the same `complete` on the same client) sends it; the keepalive
/// on that same client does not (docs/transport.md). The keepalive sends thinking
/// disabled — `max_tokens: 0` rejects thinking-on — and `xhigh`/`max` are illegal
/// with thinking off, so the ping cannot carry the field the turns do. Its
/// envelope therefore differs from a turn's by exactly this key whenever a session
/// sets an effort; whether that splits the provider's cache key is the watch-item
/// in docs/transport.md, read off `usage.jsonl`.
///
/// A client that was never given an effort — the spotter's, built by
/// `spotter::ForkSpot` with a bare `Client::new` — sends the field on neither
/// path, which the second half asserts.
#[tokio::test]
async fn only_the_shared_turn_envelope_carries_the_effort() {
    use std::io::{Read, Write};

    /// One request/response over a fresh socket; returns the request bytes the
    /// client sent. `sse` picks the canned reply: an empty stream for `complete`,
    /// a usage-only JSON body for `keepalive`.
    async fn round_trip(effort: Option<&str>, sse: bool) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            // Read until the whole POST body has arrived: one `read` can return the
            // headers alone, and the assertion below is about the body.
            let mut request = String::new();
            let mut buf = [0u8; 16384];
            loop {
                let read = sock.read(&mut buf).unwrap();
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
                let complete = request.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    head.to_ascii_lowercase()
                        .split("\r\n")
                        .find_map(|line| line.strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .is_some_and(|n| body.len() >= n)
                });
                if read == 0 || complete {
                    break;
                }
            }
            let (content_type, body) = if sse {
                (
                    "text/event-stream",
                    "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
                     \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                     event: data\ndata: [DONE]\n\n"
                        .to_string(),
                )
            } else {
                (
                    "application/json",
                    "{\"id\":\"msg-1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\
                     \"output_tokens\":0}}"
                        .to_string(),
                )
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
            request
        });

        let endpoint = Endpoint {
            provider: Provider::Anthropic,
            url: format!("http://{addr}/v1/messages"),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
        };
        let client = Client::new(endpoint, "claude-opus-5", "sys", None)
            .with_effort(effort.map(String::from));
        if sse {
            client
                .complete(&[], &[], ToolChoice::auto(), |_| {})
                .await
                .expect("the canned stream assembles");
        } else {
            client
                .keepalive(&[], &[], ToolChoice::auto())
                .await
                .expect("the canned body parses");
        }
        server.join().unwrap()
    }

    let turn = round_trip(Some("max"), true).await;
    assert!(
        turn.contains(r#""output_config":{"effort":"max"}"#),
        "the turn envelope carries it: {turn}"
    );
    let ping = round_trip(Some("max"), false).await;
    assert!(
        !ping.contains("output_config"),
        "the keepalive does not: {ping}"
    );

    // The shape of the spotter's client: never given an effort, so neither path
    // carries one — including with the forced tool choice that disables thinking.
    let turn = round_trip(None, true).await;
    assert!(!turn.contains("output_config"), "{turn}");
    let ping = round_trip(None, false).await;
    assert!(!ping.contains("output_config"), "{ping}");
}

/// The attribution headers ride an OpenRouter inference POST and no other
/// (docs/transport.md). Asserted over a socket rather than off
/// `Provider::attribution` alone, because the thing that can break is the wiring:
/// `with_attribution` is applied on two conn-building paths and would be silently
/// absent from either.
///
/// The negative half is the point. `HTTP-Referer` is a header hosts act on with
/// its RFC 9110 meaning, so an Anthropic turn must carry none of these — while
/// still carrying the `User-Agent`, which every host is meant to see.
#[tokio::test]
async fn attribution_rides_openrouter_turns_and_nothing_else() {
    use std::io::{Read, Write};

    /// The request head of one round trip against a canned reply. `sse` picks
    /// the path: an empty stream for `complete`, a usage-only body for
    /// `keepalive`.
    async fn request_head(provider: Provider, sse: bool) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut request = String::new();
            let mut buf = [0u8; 16384];
            while !request.contains("\r\n\r\n") {
                let read = sock.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
            }
            let (content_type, body) = if sse {
                (
                    "text/event-stream",
                    "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
                     \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                     event: data\ndata: [DONE]\n\n",
                )
            } else {
                (
                    "application/json",
                    "{\"id\":\"msg-1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\
                     \"output_tokens\":0}}",
                )
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
            request
        });

        let endpoint = Endpoint {
            provider,
            url: format!("http://{addr}/v1/messages"),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
        };
        let client = Client::new(endpoint, "m", "sys", None);
        if sse {
            client
                .complete(&[], &[], ToolChoice::auto(), |_| {})
                .await
                .expect("the canned stream assembles");
        } else {
            client
                .keepalive(&[], &[], ToolChoice::auto())
                .await
                .expect("the canned body parses");
        }
        let request = server.join().unwrap();
        request
            .split_once("\r\n\r\n")
            .map_or(request.clone(), |(head, _)| head.to_string())
            .to_ascii_lowercase()
    }

    // The `User-Agent` is the one header every request carries, on every path.
    let expected_ua = format!("user-agent: {}", crate::http::user_agent()).to_ascii_lowercase();

    for sse in [true, false] {
        let openrouter = request_head(Provider::OpenRouter, sse).await;
        for header in [
            "http-referer: https://efference.net",
            "x-openrouter-title: efference",
            "x-openrouter-categories: cli-agent,programming-app",
        ] {
            assert!(
                openrouter.contains(header),
                "missing {header}: {openrouter}"
            );
        }
        assert!(openrouter.contains(&expected_ua), "{openrouter}");

        let anthropic = request_head(Provider::Anthropic, sse).await;
        assert!(!anthropic.contains("referer"), "{anthropic}");
        assert!(!anthropic.contains("x-openrouter"), "{anthropic}");
        assert!(anthropic.contains(&expected_ua), "{anthropic}");
    }
}

/// The Codex backend's turn-routing contract over a socket (docs/transport.md):
/// the first request of a turn carries no token, the token the backend issues
/// on that response rides every later request of the turn unchanged (a second
/// issued value does not replace it), and a new turn starts without one. The
/// model hint rides every request. Three connections, each closed by the server,
/// so the pool opens a fresh one per request and the mock sees each head whole.
#[tokio::test]
async fn codex_turn_routing_token_is_replayed_within_a_turn_and_dropped_at_the_next() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let issued = [Some("tok-one"), Some("tok-two"), None];
        let mut heads = Vec::new();
        for token in issued {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let n = sock.read(&mut buf).unwrap();
            heads.push(String::from_utf8_lossy(&buf[..n]).to_string());
            let body = "event: response.completed\ndata: {\"type\":\"response.completed\",\
                        \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
                        \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
            let token_line =
                token.map_or(String::new(), |t| format!("x-codex-turn-state: {t}\r\n"));
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n{token_line}\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
        heads
    });

    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: format!("http://{addr}/backend-api/codex/responses"),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
    };
    let client = Client::new(endpoint, "gpt-5.6-sol", "sys", None);
    assert_eq!(client.turn_routing_token(), None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(client.turn_routing_token().as_deref(), Some("tok-one"));
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        client.turn_routing_token().as_deref(),
        Some("tok-one"),
        "the first issued token holds for the whole turn"
    );
    client.begin_turn();
    assert_eq!(client.turn_routing_token(), None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let heads = server.join().unwrap();

    let head = |i: usize| {
        heads[i]
            .split("\r\n\r\n")
            .next()
            .unwrap()
            .to_ascii_lowercase()
    };
    for i in 0..3 {
        assert!(
            head(i).contains("x-codex-routing-hint: model=gpt-5.6-sol"),
            "request {i} names the model for the router"
        );
    }
    assert!(
        !head(0).contains("x-codex-turn-state"),
        "a turn's first request carries no token"
    );
    assert!(
        head(1).contains("x-codex-turn-state: tok-one"),
        "the second request replays it"
    );
    assert!(
        !head(2).contains("x-codex-turn-state"),
        "a new turn starts without one"
    );
}

/// A client outside any turn (the spotter's) never holds a token, whatever the
/// backend sends.
#[test]
fn a_client_without_turn_routing_holds_no_token() {
    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: "http://127.0.0.1:1/x".into(),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
    };
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys", None).without_turn_routing();
    client.begin_turn();
    assert_eq!(client.turn_routing_token(), None);
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

/// The transport's own retry gate reads the non-2xx body (docs/transport.md): a
/// 429 whose body names an exhausted plan is answered once, where the status
/// alone would be retried. The mock counts connections; each closes after one
/// response.
#[tokio::test]
async fn a_terminal_429_body_is_answered_once() {
    use std::io::{Read, Write};

    async fn attempts_for(body: &'static str) -> usize {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = 0;
            loop {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let mut buf = vec![0u8; 65536];
                let n = sock.read(&mut buf).unwrap_or(0);
                // The unblocking connect below sends nothing: it ends the loop.
                if !buf[..n].starts_with(b"POST") {
                    break;
                }
                sock.write_all(
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\n\
                         connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
                sock.flush().unwrap();
                served += 1;
                if served == 12 {
                    break;
                }
            }
            served
        });
        let endpoint = Endpoint {
            provider: Provider::OpenAICodex,
            url: format!("http://{addr}/backend-api/codex/responses"),
            auth_header: "authorization".into(),
            auth_value: "Bearer k".into(),
            session_id: None,
            extra_headers: Vec::new(),
        };
        let client = Client::new(endpoint, "gpt-5.6-sol", "sys", None);
        let outcome = client.complete(&[], &[], ToolChoice::auto(), |_| {}).await;
        assert!(outcome.is_err(), "a 429 that never clears is an error");
        drop(client);
        // Unblock the accept loop if it is still waiting.
        let _ = std::net::TcpStream::connect(addr);
        server.join().unwrap()
    }

    let terminal = attempts_for(
        r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1788744444,"resets_in_seconds":15805}}"#,
    )
    .await;
    assert_eq!(terminal, 1, "an exhausted plan is answered once");
}
