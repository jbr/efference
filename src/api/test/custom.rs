//! The custom provider: its dialect-derived behaviour, its URLs, and the
//! opportunistic listing parse (docs/transport.md).

use super::*;

const MESSAGES: Provider = Provider::Custom(WireDialect::Messages);
const RESPONSES: Provider = Provider::Custom(WireDialect::Responses);
const SYSTEM_ONE: Provider = Provider::Custom(WireDialect::SystemOne);

/// The bare OpenAI-shaped listing, as Meta's Model API answers it: an id and
/// nothing a capability consumer can use.
const BARE: &str = r#"{"object": "list", "data": [
    {"id": "muse-spark-1.3", "object": "model", "created": 0, "owned_by": "meta"}
]}"#;

/// A capability-rich listing, as Inception answers it: the window, the output
/// cap, modalities, cache pricing, and the accepted sampling parameters.
const RICH: &str = r#"{"data": [
    {"id": "mercury-2.5", "context_length": 260000, "max_output_length": 65536,
     "input_modalities": ["text"], "output_modalities": ["text"],
     "pricing": {"prompt": "0.00000004", "completion": "0.00000015",
                 "input_cache_reads": "0.000000004", "input_cache_writes": "0"},
     "supported_sampling_parameters": ["temperature", "stop"],
     "supported_features": ["tools", "json_mode", "structured_outputs"]}
]}"#;

fn caps(body: &str, model: &str) -> ModelCapabilities {
    MESSAGES.parse_capabilities(body, model, &[]).unwrap()
}

/// A listing that names the model but declares nothing about it is a valid
/// answer: every capability reads unknown, and none of them is guessed.
#[test]
fn a_bare_listing_declares_no_capabilities() {
    let caps = caps(BARE, "muse-spark-1.3");
    assert_eq!(caps.context_length, None);
    assert_eq!(caps.max_output_tokens, None);
    assert!(!caps.supports_image_input);
    assert_eq!(caps.prompt_cache, PromptCachePolicy::None);
    assert_eq!(caps.supported_efforts, None);
    // Unknown asserts nothing on either axis that would otherwise make the
    // harness send something: temperature passes through to the endpoint's own
    // validation, and the reasoning requirement stays unstated so thinking is
    // omitted rather than explicitly disabled.
    assert!(caps.accepts_temperature);
    assert_eq!(caps.reasoning_mandatory, None);
    assert!(!caps.supports_reasoning);
}

/// Every field a rich listing declares is read, from the spellings a real endpoint
/// uses rather than a normalized shape invented here.
#[test]
fn a_rich_listing_fills_in_what_it_declares() {
    let caps = caps(RICH, "mercury-2.5");
    assert_eq!(caps.context_length, Some(260_000));
    assert_eq!(caps.max_output_tokens, Some(65_536));
    // `input_modalities` names only text, so image input is a declared no rather
    // than an unknown one.
    assert!(!caps.supports_image_input);
    assert!(caps.accepts_temperature);
    // A read price with a zero write price is a cache the provider fills itself.
    assert_eq!(caps.prompt_cache, PromptCachePolicy::Implicit);
}

/// A model the listing does not name is unknown, not an error: a listing can be
/// paginated or omit an alias the endpoint still serves, so it is not a statement
/// about what the endpoint will accept.
#[test]
fn a_model_absent_from_the_listing_is_unknown_not_refused() {
    let absent = caps(RICH, "mercury-9");
    assert_eq!(absent.context_length, None);
    assert_eq!(absent.max_output_tokens, None);
    // An empty listing and an unparseable one are different: the first says
    // nothing, the second is not a listing.
    assert_eq!(caps(r#"{"data": []}"#, "m").context_length, None);
    assert!(MESSAGES.parse_capabilities("not json", "m", &[]).is_err());
}

/// How a listing discloses a prompt cache is its cache *pricing*, in either
/// spelling, and a nonzero write price is what distinguishes a cache the caller
/// writes from one the provider fills.
#[test]
fn cache_pricing_decides_the_cache_policy() {
    let policy = |pricing: &str| {
        caps(
            &format!(r#"{{"data": [{{"id": "m", "pricing": {pricing}}}]}}"#),
            "m",
        )
        .prompt_cache
    };
    assert_eq!(policy(r#"{"prompt": "1"}"#), PromptCachePolicy::None);
    assert_eq!(
        policy(r#"{"input_cache_read": "0.1"}"#),
        PromptCachePolicy::Implicit
    );
    assert_eq!(
        policy(r#"{"input_cache_reads": "0.1", "input_cache_writes": "0"}"#),
        PromptCachePolicy::Implicit
    );
    assert_eq!(
        policy(r#"{"input_cache_read": "0.1", "input_cache_write": "0.2"}"#),
        PromptCachePolicy::Explicit
    );
    // No pricing block at all is no evidence of a cache, which disarms the
    // keepalive rather than pinging an endpoint that may reject the primitive.
    assert_eq!(
        caps(BARE, "muse-spark-1.3").prompt_cache,
        PromptCachePolicy::None
    );
}

/// Modalities are read flat or nested, since one listing nests them under
/// `architecture` and another does not.
#[test]
fn image_input_is_read_from_either_modality_shape() {
    let images = |entry: &str| {
        caps(&format!(r#"{{"data": [{{"id": "m", {entry}}}]}}"#), "m").supports_image_input
    };
    assert!(images(r#""input_modalities": ["text", "image"]"#));
    assert!(images(
        r#""architecture": {"input_modalities": ["text", "image"]}"#
    ));
    assert!(!images(r#""input_modalities": ["text"]"#));
}

/// The dialect picks the route, and the base's trailing slash is optional
/// because both spellings of one endpoint are what an operator will paste.
#[test]
fn the_dialect_picks_the_route_off_the_configured_base() {
    let base = Some("https://api.example.com/v1");
    assert_eq!(
        MESSAGES.messages_url(base).as_deref(),
        Some("https://api.example.com/v1/messages")
    );
    assert_eq!(
        RESPONSES.messages_url(base).as_deref(),
        Some("https://api.example.com/v1/responses")
    );
    assert_eq!(
        MESSAGES
            .messages_url(Some("https://api.example.com/v1/"))
            .as_deref(),
        Some("https://api.example.com/v1/messages")
    );
    // Capabilities and the credential check are the same listing route on both
    // dialects — one request answers the model menu, the key, and what little
    // the endpoint says about the model.
    for provider in [MESSAGES, RESPONSES] {
        assert_eq!(
            provider.capabilities_url(base, "any").unwrap(),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            provider.key_check_url(base).as_deref(),
            Some("https://api.example.com/v1/models")
        );
        // A listing carries no effort levels, and there is no second route to ask
        // at an endpoint this crate did not choose.
        assert_eq!(provider.effort_levels_url("any").unwrap(), None);
    }
}

/// A custom endpoint's listing declares no effort levels, so a configured level
/// passes through unvalidated rather than refused: nothing affirmed any level to
/// check it against (docs/transport.md).
#[test]
fn a_custom_listing_leaves_a_configured_effort_unvalidated() {
    let caps = caps(
        r#"{"data":[{"id":"m","max_input_tokens":1000,"max_tokens":100}]}"#,
        "m",
    );
    assert_eq!(caps.supported_efforts, None);
    assert!(!caps.validates_efforts);
    assert!(caps.check_effort("high", "m").is_ok());
}

/// Every vendor-specific header and body field is withheld from a custom
/// endpoint. Each of these is a fact about one deployment, and an unrecognized
/// header or top-level key may be rejected rather than ignored.
#[test]
fn a_custom_endpoint_is_sent_no_vendor_specific_extras() {
    for provider in [MESSAGES, RESPONSES] {
        assert_eq!(provider.auth_header(), "authorization");
        assert_eq!(provider.auth_header_value("k"), "Bearer k");
        assert_eq!(provider.session_header(), None);
        assert_eq!(provider.turn_routing_header(), None);
        assert_eq!(provider.routing_hint_header(), None);
        assert_eq!(provider.attribution(), &[]);
        assert!(!provider.reports_plan_windows());
        assert_eq!(provider.decisions_url(Some("https://x.example")), None);
        assert!(provider.serves_inference());
        // 0..1 is the range both scales accept, so no value sent is out of range.
        assert_eq!(provider.map_temperature(100), 1.0);
    }
    // `prompt_cache_key` is an OpenAI-dialect field, and an unrecognized
    // top-level key is a 400 on Messages.
    assert!(!MESSAGES.prompt_cache_key_field());
    assert!(RESPONSES.prompt_cache_key_field());
}

/// The dialect is part of the reported identity: the bare word "custom" would
/// not say what the session speaks.
#[test]
fn the_reported_provider_name_carries_the_dialect() {
    assert_eq!(MESSAGES.name(), "custom-messages");
    assert_eq!(RESPONSES.name(), "custom-responses");
    assert_eq!(
        serde_json::to_value(MESSAGES).unwrap(),
        serde_json::json!("custom-messages")
    );
    assert_eq!(MESSAGES.inference_dialect(), Some(WireDialect::Messages));
    assert_eq!(RESPONSES.inference_dialect(), Some(WireDialect::Responses));
}

/// A base URL belongs to a custom connection and to no other: a named provider
/// given one would be silently ignoring it, and a custom one without it has no
/// route to build.
#[test]
fn a_base_url_is_required_for_custom_and_refused_elsewhere() {
    let endpoint = Endpoint::from_credentials(
        MESSAGES,
        "tok".into(),
        None,
        Some("https://api.example.com/v1".into()),
    )
    .unwrap();
    assert_eq!(
        endpoint.url.as_deref(),
        Some("https://api.example.com/v1/messages")
    );
    assert_eq!(endpoint.base.as_deref(), Some("https://api.example.com/v1"));

    let missing = Endpoint::from_credentials(MESSAGES, "tok".into(), None, None).unwrap_err();
    assert!(missing.to_string().contains("base URL"), "{missing}");

    let unwanted = Endpoint::from_credentials(
        Provider::Anthropic,
        "tok".into(),
        None,
        Some("https://api.example.com/v1".into()),
    )
    .unwrap_err();
    assert!(unwanted.to_string().contains("own URLs"), "{unwanted}");
}

/// The dialect vocabulary is closed, because a dialect selects an encoder and an
/// assembler that exist in this crate.
#[test]
fn the_dialect_name_vocabulary_is_closed() {
    assert_eq!(
        WireDialect::from_name("messages").unwrap(),
        WireDialect::Messages
    );
    assert_eq!(
        WireDialect::from_name(" responses ").unwrap(),
        WireDialect::Responses
    );
    assert_eq!(
        WireDialect::from_name("systemone").unwrap(),
        WireDialect::SystemOne
    );
    assert_eq!(WireDialect::SystemOne.config_name(), "systemone");
    let error = WireDialect::from_name("completions")
        .unwrap_err()
        .to_string();
    assert!(error.contains("completions"), "{error}");
    assert!(error.contains("messages"), "{error}");
}

/// A custom `systemone` connection speaks the decision-model contract on the URL
/// it was given, whole: providers that adopted the contract spell its path
/// differently, so no segment is appended. It has no turns route, no listing, and
/// no capability or key-check route, and serves no inference.
#[test]
fn a_custom_system_one_endpoint_posts_to_its_url_whole() {
    let url = "https://api.cloudflare.example/client/v4/accounts/a1/ai/run/@cf/cloudflare/clef";
    assert_eq!(SYSTEM_ONE.decisions_url(Some(url)).as_deref(), Some(url));
    assert_eq!(SYSTEM_ONE.decisions_url(None), None);
    assert_eq!(SYSTEM_ONE.name(), "custom-systemone");
    assert_eq!(SYSTEM_ONE.inference_dialect(), None);
    assert!(!SYSTEM_ONE.serves_inference());
    assert_eq!(SYSTEM_ONE.messages_url(Some(url)), None);
    assert_eq!(SYSTEM_ONE.models_url(Some(url)), None);
    assert_eq!(SYSTEM_ONE.key_check_url(Some(url)), None);
    assert!(SYSTEM_ONE.capabilities_url(Some(url), "clef").is_err());
    // Vendor-specific extras stay withheld, as for the turn dialects.
    assert_eq!(SYSTEM_ONE.auth_header_value("k"), "Bearer k");
    assert!(!SYSTEM_ONE.decisions_takes_session_id());
    assert_eq!(SYSTEM_ONE.attribution(), &[]);

    let endpoint =
        Endpoint::from_credentials(SYSTEM_ONE, "tok".into(), None, Some(url.into())).unwrap();
    assert_eq!(endpoint.url, None);
    assert_eq!(endpoint.base.as_deref(), Some(url));
}
