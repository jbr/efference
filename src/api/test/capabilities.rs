//! Tests for per-provider capability parsing and aggregation, the upstream
//! pin, prompt-caching metadata, image-input defaults, and the exact
//! model capability profiles.

use super::*;

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
    let caps = Provider::Anthropic
        .parse_capabilities(body, "m", &[])
        .unwrap();
    assert_eq!(caps.context_length, Some(200000));
    assert_eq!(caps.max_output_tokens, Some(64000));
}

/// A Models API response without `max_input_tokens` fails the (required) boot
/// lookup rather than defaulting — the ceiling would otherwise be a guess.
#[test]
fn anthropic_capabilities_missing_context_is_an_error() {
    let err = Provider::Anthropic
        .parse_capabilities(r#"{"id": "m", "display_name": "M"}"#, "m", &[])
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
    let caps = Provider::OpenRouter
        .parse_capabilities(body, "m", &[])
        .unwrap();
    assert_eq!(caps.context_length, Some(200000));
    assert_eq!(caps.max_output_tokens, Some(48000));
    // No endpoint above lists `supported_parameters` at all → not granted.
    assert!(!caps.thinking.adaptive);
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
            .parse_capabilities(&body(caps), "m", &[])
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

/// Adaptive thinking is min-style like the ceilings: granted only when every
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
            .parse_capabilities(all, "m", &[])
            .unwrap()
            .thinking
            .adaptive
    );
    let mixed = r#"{"data": {"endpoints": [
        {"context_length": 1000000, "supported_parameters": ["reasoning", "tools"]},
        {"context_length": 1000000, "supported_parameters": ["tools"]}
    ]}}"#;
    assert!(
        !Provider::OpenRouter
            .parse_capabilities(mixed, "m", &[])
            .unwrap()
            .thinking
            .adaptive
    );
}

/// A model no endpoint serves (or that reports no context length at all) fails
/// the boot lookup with a legible message.
#[test]
fn openrouter_capabilities_no_endpoints_is_an_error() {
    let err = Provider::OpenRouter
        .parse_capabilities(r#"{"data": {"endpoints": []}}"#, "m", &[])
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
        .capabilities_url(None, "claude-haiku-4-5")
        .unwrap_err()
        .to_string();
    assert!(err.contains("author/slug"), "{err}");
    let url = Provider::OpenRouter
        .capabilities_url(None, "anthropic/claude-haiku-4.5")
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
            .parse_capabilities(body, "m", upstream)
            .unwrap()
    };
    let unpinned = caps(&[]);
    assert_eq!(unpinned.context_length, Some(1_000_000));
    // 8_000 is below `OUTPUT_FLOOR`, so the unpinned min skips it.
    assert_eq!(unpinned.max_output_tokens, Some(64_000));

    let pinned = caps(&["google-ai-studio".to_string()]);
    assert_eq!(pinned.context_length, Some(200_000));
    assert_eq!(pinned.max_output_tokens, Some(64_000));

    let unknown = caps(&["not-an-endpoint".to_string()]);
    assert_eq!(unknown.context_length, Some(1_000_000));
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
            .parse_capabilities(
                &format!(r#"{{"data": {{"endpoints": {endpoints}}}}}"#),
                "m",
                &[],
            )
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
            .parse_capabilities(r#"{"max_input_tokens": 200000}"#, "m", &[])
            .unwrap()
            .prompt_cache,
        PromptCachePolicy::Explicit,
    );
}

/// The platform arm reads the subscription backend's catalog, which answers a
/// platform key and reports what the platform's own Models API does not: the
/// window, the reasoning levels, the modalities (docs/transport.md). The cache
/// policy stays the platform's own — explicit, where the Codex arm's is
/// implicit — because the catalog reports none. A model the catalog does not
/// name boots on that baseline rather than failing.
#[tokio::test]
async fn the_platform_arm_reads_the_catalog_over_its_own_baseline() {
    super::seed_codex_catalog(Provider::OpenAI, "");
    let platform = Endpoint::from_credentials(Provider::OpenAI, "sk-test".into(), None, None)
        .expect("a platform connection is well-formed");
    let named = platform
        .fetch_capabilities("gpt-5.6-luna", &[])
        .await
        .unwrap();
    assert_eq!(named.context_length, Some(272_000));
    assert_eq!(named.default_effort.as_deref(), Some("medium"));
    assert!(named.supports_image_input);
    assert_eq!(named.prompt_cache, PromptCachePolicy::Explicit);
    assert!(!named.validates_efforts);

    let unnamed = platform
        .fetch_capabilities("gpt-9-luna", &[])
        .await
        .unwrap();
    assert_eq!(unnamed.context_length, None);
    assert_eq!(unnamed.prompt_cache, PromptCachePolicy::Explicit);
}

/// The spotter default each arm falls back to, for a connection that names none
/// of its own (docs/permissions.md).
#[test]
fn each_arm_names_its_own_default_spotter() {
    // One default per arm, not a catalog: the spotter shares the endpoint and
    // wants the cheap band, and no source reports which model that is.
    assert_eq!(
        Provider::OpenAI.default_spotter_model(),
        Some("gpt-5.6-luna")
    );
    assert_eq!(
        Provider::OpenAICodex.default_spotter_model(),
        Some("gpt-5.6-luna")
    );
    assert_eq!(
        Provider::Anthropic.default_spotter_model(),
        Some("claude-haiku-4-5")
    );
    assert_eq!(
        Provider::OpenRouter.default_spotter_model(),
        Some("anthropic/claude-haiku-4-5")
    );
    // A custom endpoint publishes no catalog to pick from, so resolution asks for
    // `[defaults].spotter` rather than guessing a slug.
    assert_eq!(
        Provider::Custom(WireDialect::Messages).default_spotter_model(),
        None
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
                "m",
                &[],
            )
            .unwrap()
            .supports_image_input
    };
    assert!(openrouter(r#"["text", "image"]"#));
    assert!(!openrouter(r#"["text"]"#));
    assert!(
        !Provider::OpenRouter
            .parse_capabilities(
                r#"{"data": {"endpoints": [{"context_length": 1}]}}"#,
                "m",
                &[]
            )
            .unwrap()
            .supports_image_input,
        "no architecture at all reads as text-only"
    );

    let anthropic = |body: &str| {
        Provider::Anthropic
            .parse_capabilities(body, "m", &[])
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

/// The keepalive's prices are the conservative reading of the endpoints that price a
/// cache read (docs/transport.md): the smallest cold-read premium, the largest
/// completion price, narrowed by the `--upstream` pin first. Endpoints with no
/// cache-read price, or an unparseable one, contribute nothing; with none left the
/// prices are absent and the keepalive uses its token-ratio rule.
#[test]
fn keepalive_prices_are_the_conservative_endpoint_reading() {
    let body = r#"{
        "data": {
            "endpoints": [
                {"tag": "z-ai", "context_length": 200000,
                 "pricing": {"prompt": "0.00000015", "completion": "0.0000005",
                             "input_cache_read": "0.00000003"}},
                {"tag": "other", "context_length": 200000,
                 "pricing": {"prompt": "0.0000002", "completion": "0.000002",
                             "input_cache_read": "0.00000018"}},
                {"tag": "uncached", "context_length": 200000,
                 "pricing": {"prompt": "0.0000001", "completion": "0.0000009"}}
            ]
        }
    }"#;
    let prices = |upstream: &[String]| {
        Provider::OpenRouter
            .parse_capabilities(body, "m", upstream)
            .unwrap()
            .prices
    };
    let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
    let all = prices(&[]).unwrap();
    // The 'other' endpoint's premium (0.02/M) is the smaller; the largest
    // completion price among the caching endpoints is 2/M.
    assert!(close(all.prompt, 0.2e-6) && close(all.cache_read, 0.18e-6));
    assert!(close(all.completion, 2e-6));

    let pinned = prices(&["z-ai".to_string()]).unwrap();
    assert!(close(pinned.prompt, 0.15e-6) && close(pinned.completion, 0.5e-6));
    assert!(close(pinned.premium(), 0.12e-6));

    assert_eq!(prices(&["uncached".to_string()]), None);
    // The display-string spelling of a cache-read price is not a per-token price.
    let display = r#"{"data": {"endpoints": [{"context_length": 1,
        "pricing": {"prompt": "0.1", "completion": "0.2",
                    "input_cache_read": "$0.1/M tokens"}}]}}"#;
    let caps = Provider::OpenRouter
        .parse_capabilities(display, "m", &[])
        .unwrap();
    assert_eq!(caps.prices, None);
}
