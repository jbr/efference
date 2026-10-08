//! Tests for the subscription backend's model catalog: what an entry reports,
//! what a model the catalog does not name gets, and what a menu is offered
//! (docs/transport.md).
//!
//! The bodies here are trimmed from what the route answered on 2026-09-24,
//! field for field where the parse reads them, so a shape the backend stopped
//! sending shows up as a parse that stops filling a field rather than as a
//! passing test over a fixture that never matched reality.

use super::*;

const CATALOG: &str = r#"{
  "models": [
    {
      "slug": "gpt-6-sol",
      "display_name": "GPT-6-Sol",
      "context_window": 272000,
      "max_context_window": 872000,
      "input_modalities": ["text", "image"],
      "default_reasoning_level": "medium",
      "supported_reasoning_levels": [
        {"effort": "low", "description": "Fast"},
        {"effort": "medium", "description": "Balanced"},
        {"effort": "ultra", "description": "With delegation"}
      ],
      "visibility": "list",
      "supported_in_api": true
    },
    {
      "slug": "gpt-5.5",
      "display_name": "GPT-5.5",
      "context_window": 272000,
      "input_modalities": ["text"],
      "default_reasoning_level": "medium",
      "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}],
      "visibility": "list",
      "supported_in_api": true
    },
    {
      "slug": "gpt-reserve",
      "display_name": "GPT-Reserve",
      "context_window": 272000,
      "input_modalities": ["text", "image"],
      "supported_reasoning_levels": [{"effort": "low"}],
      "visibility": "hide",
      "supported_in_api": true
    }
  ]
}"#;

/// An entry speaks for the fields it carries: the window the backend says the
/// model works in, its levels (including one no compiled table named), its
/// default, and its modalities. The fields it does not carry keep the baseline,
/// so a missing output cap reads as unreported rather than as a cap of zero, and
/// a temperature the catalog is silent about passes through for the provider to
/// accept or refuse.
#[test]
fn an_entry_reports_its_window_levels_and_modalities() {
    let catalog = parse(CATALOG).unwrap();
    let caps = catalog
        .entry("gpt-6-sol")
        .unwrap()
        .capabilities(baseline(Provider::OpenAICodex));
    // The catalog's window, not the larger public figure: what the backend
    // reports is what the backend enforces.
    assert_eq!(caps.context_length, Some(272_000));
    assert_eq!(
        caps.supported_efforts.as_deref(),
        Some(&["low", "medium", "ultra"].map(String::from)[..])
    );
    assert_eq!(caps.default_effort.as_deref(), Some("medium"));
    assert!(caps.thinking.adaptive);
    assert!(caps.supports_image_input);
    // The levels are reported, not enforced: the catalog lists Codex CLI's modes
    // rather than the wire's values, so no configured level is refused here.
    assert!(!caps.validates_efforts);
    assert!(caps.check_effort("ultra", "gpt-6-sol").is_ok());
    assert!(caps.check_effort("xhigh", "gpt-6-sol").is_ok());
    assert!(caps.check_effort("none", "gpt-6-sol").is_ok());
    // The arm's own facts, over which the entry is read. The baseline is the
    // production one, and the cache policy is the arm's assertion rather than
    // anything an entry says: the backend 400s every explicit cache parameter
    // on every model it serves.
    assert_eq!(
        baseline(Provider::OpenAICodex).prompt_cache,
        PromptCachePolicy::Implicit
    );
    assert_eq!(
        baseline(Provider::OpenAI).prompt_cache,
        PromptCachePolicy::Explicit
    );
    assert_eq!(caps.prompt_cache, PromptCachePolicy::Implicit);
    assert_eq!(caps.max_output_tokens, None);
    assert!(caps.accepts_temperature);
    assert_eq!(caps.thinking.off, ThinkingOff::Omit);

    // A text-only entry claims no image input, and reports its levels without
    // enforcing any of them.
    let text_only = catalog
        .entry("gpt-5.5")
        .unwrap()
        .capabilities(baseline(Provider::OpenAICodex));
    assert!(!text_only.supports_image_input);
    assert!(text_only.check_effort("ultra", "gpt-5.5").is_ok());
}

/// A model the catalog does not name is not an error and not a guess: it passes
/// boot on the arm's baseline, with no effort validation to refuse a configured
/// level over. This is what keeps a model released after the build usable — the
/// turn is the test of whether the provider serves it.
#[test]
fn a_model_the_catalog_does_not_name_passes_on_the_baseline() {
    let catalog = parse(CATALOG).unwrap();
    assert!(catalog.entry("gpt-9-codex").is_none());
    let caps = baseline(Provider::OpenAICodex);
    assert_eq!(caps.context_length, None);
    assert!(!caps.validates_efforts);
    assert!(caps.check_effort("ultra", "gpt-9-codex").is_ok());
    // And a connection's models stay usable for everything the harness gates on:
    // the catalog is the source of the window, not a gate on the model.
    let missing: ModelCapabilities = ModelCapabilities::unknown();
    assert_eq!(missing.context_length, None);
}

/// The backend's `hide` is a menu choice, not an access boundary — a hidden
/// entry serves turns like any other — so it is left out of the suggestions and
/// nothing else. An entry the backend says it does not serve through the API is
/// left out for the same reason a menu leaves it out.
#[test]
fn hidden_and_unserved_entries_are_left_out_of_rows() {
    let catalog = parse(CATALOG).unwrap();
    assert_eq!(
        serde_json::to_string(&catalog.rows()).unwrap(),
        concat!(
            r#"[{"model":"gpt-6-sol","name":"GPT-6-Sol","context":272000,"reasoning":true,"images":true},"#,
            r#"{"model":"gpt-5.5","name":"GPT-5.5","context":272000,"reasoning":true,"images":false}]"#
        )
    );
    // Still present in the catalog: a hidden model is selectable by id.
    assert!(catalog.entry("gpt-reserve").is_some());
}

/// A body that is not this catalog is a shape question, and says so rather than
/// parsing as an empty one — an empty catalog read as a model list would be a
/// provider with no models, which is a different claim.
#[test]
fn a_body_that_is_not_a_catalog_is_refused() {
    assert!(parse("not json").is_err());
    let error = parse(r#"{"error":{"message":"no"}}"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a model catalog"), "{error}");
}

/// The read is cached so a session's boot and its spotter's lookup share one
/// round trip, and the entry stands for the interval a catalog takes to change.
/// Past it the next lookup asks again.
#[test]
fn a_cached_catalog_is_fresh_within_its_ttl_and_stale_after() {
    let read_at = std::time::Instant::now();
    let cached = Cached {
        body: CATALOG.to_string(),
        read_at,
    };
    assert!(cached.is_fresh(read_at));
    assert!(cached.is_fresh(read_at + CATALOG_TTL / 2));
    assert!(!cached.is_fresh(read_at + CATALOG_TTL));
    assert!(!cached.is_fresh(read_at + CATALOG_TTL * 2));
}

/// The route requires the version claim, and the claim is what makes the newest
/// models visible: the backend returns an empty catalog for a claim below every
/// entry's `minimal_client_version` (docs/transport.md).
#[test]
fn the_listing_route_carries_a_client_version_claim() {
    let url = Provider::OpenAICodex.models_url(None).unwrap();
    assert!(
        url.starts_with("https://chatgpt.com/backend-api/codex/models?"),
        "{url}"
    );
    assert!(
        url.contains(&format!("client_version={CLAIMED_CLIENT_VERSION}")),
        "{url}"
    );
}

#[test]
fn model_description_is_provider_prose() {
    let rows = parse(
        r#"{"models":[{"slug":"a","description":"Provider **prose**.\nSecond paragraph."}]}"#,
    )
    .unwrap()
    .rows();
    assert_eq!(
        rows[0].description.as_deref(),
        Some("Provider **prose**.\nSecond paragraph.")
    );
}
