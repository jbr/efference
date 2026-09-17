//! Tests for per-provider capability parsing and aggregation, the upstream
//! pin, prompt-caching metadata, image-input defaults, and the exact
//! model capability profiles.

use super::*;

/// The Codex backend takes the catalog's limits but not its explicit cache
/// policy: it rejects `prompt_cache_breakpoint`, so the arm reports implicit.
#[tokio::test]
async fn codex_backend_capabilities_report_implicit_caching() {
    let codex = Endpoint::from_credentials(Provider::OpenAICodex, "tok".into(), Some("a".into()))
        .unwrap()
        .fetch_capabilities("gpt-5.6-luna", &[])
        .await
        .unwrap();
    assert_eq!(codex.prompt_cache, PromptCachePolicy::Implicit);
    let platform = Endpoint::from_credentials(Provider::OpenAI, "tok".into(), None)
        .unwrap()
        .fetch_capabilities("gpt-5.6-luna", &[])
        .await
        .unwrap();
    assert_eq!(platform.prompt_cache, PromptCachePolicy::Explicit);
    assert_eq!(codex.context_length, platform.context_length);
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
        let caps = super::provider::openai_model_capabilities(Provider::OpenAI, model).unwrap();
        assert_eq!(caps.context_length, 1_050_000, "{model}");
        assert_eq!(caps.max_output_tokens, Some(128_000), "{model}");
        assert!(caps.supports_reasoning, "{model}");
        assert!(caps.supports_image_input, "{model}");
        assert_eq!(caps.prompt_cache, PromptCachePolicy::Explicit, "{model}");
        assert_eq!(caps.default_effort.as_deref(), Some("medium"), "{model}");
        assert!(caps.accepts_temperature, "{model}");
        assert_eq!(
            caps.supported_efforts.as_deref(),
            Some(&["none", "low", "medium", "high", "xhigh", "max"].map(String::from)[..]),
            "{model}",
        );
    }

    // GPT-6 Astra: the model page's window and output, no `none` effort, no
    // temperature (docs/transport.md).
    let astra =
        super::provider::openai_model_capabilities(Provider::OpenAI, "gpt-6-astra").unwrap();
    assert_eq!(astra.context_length, 1_050_000);
    assert_eq!(astra.max_output_tokens, Some(128_000));
    assert!(astra.supports_reasoning && astra.supports_image_input);
    assert!(!astra.accepts_temperature);
    assert_eq!(astra.default_effort, None);
    assert_eq!(
        astra.supported_efforts.as_deref(),
        Some(&["low", "medium", "high", "xhigh", "max"].map(String::from)[..]),
    );
    // The harness does not normalize slugs: a `.0` the catalog never named is unknown,
    // and the error lists what this build knows.
    let error = super::provider::openai_model_capabilities(Provider::OpenAI, "gpt-6.0-astra")
        .unwrap_err()
        .to_string();
    assert!(error.contains("gpt-6-astra"), "{error}");

    // The spotter default follows the catalog (docs/permissions.md).
    assert_eq!(Provider::OpenAI.default_spotter_model(), "gpt-5.6-luna");
    assert_eq!(
        Provider::OpenAICodex.default_spotter_model(),
        "gpt-5.6-luna"
    );
    assert_eq!(
        Provider::Anthropic.default_spotter_model(),
        "claude-haiku-4-5"
    );
    assert_eq!(
        Provider::OpenRouter.default_spotter_model(),
        "anthropic/claude-haiku-4-5"
    );

    let error = super::provider::openai_model_capabilities(Provider::OpenAI, "gpt-5.7-luna")
        .unwrap_err()
        .to_string();
    assert!(error.contains("no capability profile"), "{error}");
    assert!(error.contains("gpt-5.7-luna"), "{error}");
}

/// Spark is a text-only Codex subscription model. It must not leak into the
/// platform API catalog, where the same model id is unavailable.
#[test]
fn codex_spark_capability_profile_is_provider_specific() {
    let spark =
        super::provider::openai_model_capabilities(Provider::OpenAICodex, "gpt-5.3-codex-spark")
            .unwrap();
    assert_eq!(spark.context_length, 128_000);
    assert_eq!(spark.max_output_tokens, None);
    assert!(spark.supports_reasoning);
    assert!(!spark.supports_image_input);
    assert!(!spark.accepts_temperature);
    assert_eq!(spark.default_effort.as_deref(), Some("medium"));
    assert_eq!(
        spark.supported_efforts.as_deref(),
        Some(&["low", "medium", "high", "xhigh"].map(String::from)[..]),
    );

    let error = super::provider::openai_model_capabilities(Provider::OpenAI, "gpt-5.3-codex-spark")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("only on the openai-codex provider"),
        "{error}"
    );
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
