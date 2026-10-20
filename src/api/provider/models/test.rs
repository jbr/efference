//! Tests for the `openai` platform menu (docs/model-catalog.md), over bodies
//! shaped like the two it joins: the platform listing (`id`, `object`,
//! `created`, `owned_by`, and `shutdown_date` where a model has one) and the
//! Codex catalog.

use super::*;

const LISTING: &str = r#"{
  "object": "list",
  "data": [
    {"id": "text-embedding-4", "object": "model", "created": 1780000000, "owned_by": "system"},
    {"id": "gpt-6-luna", "object": "model", "created": 1790000000, "owned_by": "system"},
    {"id": "gpt-5.5", "object": "model", "created": 1785000000, "owned_by": "system"},
    {"id": "gpt-6-sol", "object": "model", "created": 1790500000, "owned_by": "system"},
    {"id": "gpt-reserve", "object": "model", "created": 1791000000, "owned_by": "system"},
    {"id": "gpt-5.1-codex", "object": "model", "created": 1760000000, "owned_by": "system", "shutdown_date": "2026-07-23"},
    {"id": "sora-2", "object": "model", "created": 1770000000, "owned_by": "system", "shutdown_date": "2026-12-01"},
    {"id": "davinci-002", "object": "model", "created": 1700000000, "owned_by": "system", "shutdown_date": "2026-10-19"}
  ]
}"#;

const CATALOG: &str = r#"{
  "models": [
    {
      "slug": "gpt-6-sol",
      "display_name": "GPT-6-Sol",
      "description": "The frontier model.",
      "context_window": 272000,
      "input_modalities": ["text", "image"],
      "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}],
      "visibility": "list",
      "supported_in_api": true
    },
    {
      "slug": "gpt-reserve",
      "display_name": "GPT-Reserve",
      "context_window": 272000,
      "input_modalities": ["text"],
      "visibility": "hide",
      "supported_in_api": true
    },
    {
      "slug": "gpt-6-unlisted",
      "display_name": "GPT-6-Unlisted",
      "context_window": 272000,
      "input_modalities": ["text"],
      "visibility": "list",
      "supported_in_api": true
    },
    {
      "slug": "gpt-5.5",
      "display_name": "GPT-5.5",
      "context_window": 128000,
      "input_modalities": ["text"],
      "visibility": "list",
      "supported_in_api": true
    }
  ]
}"#;

fn models(rows: &[ListedModel]) -> Vec<&str> {
    rows.iter().map(|row| row.model.as_str()).collect()
}

/// The menu is the catalog's suggested entries the listing also names, in the
/// catalog's order (not the listing's, which dates newest first) and with the
/// catalog's fields. A hidden entry and a catalog entry the listing lacks are
/// out, and so is every listing entry the catalog does not name.
#[test]
fn the_menu_is_the_suggested_catalog_entries_the_listing_names() {
    let catalog = codex::parse(CATALOG).unwrap();
    let rows = platform_rows(LISTING, Ok(catalog), "2026-10-19").unwrap();
    assert_eq!(models(&rows), ["gpt-6-sol", "gpt-5.5"]);
    assert_eq!(
        serde_json::to_string(&rows[0]).unwrap(),
        r#"{"model":"gpt-6-sol","name":"GPT-6-Sol","description":"The frontier model.","context":272000,"reasoning":true,"images":true}"#
    );
}

/// A model off the menu is still a model: the catalog (and so the capability
/// path) names it, and a typed id is never checked against the menu.
#[test]
fn an_id_outside_the_menu_still_resolves() {
    let catalog = codex::parse(CATALOG).unwrap();
    let rows = platform_rows(LISTING, Ok(catalog.clone()), "2026-10-19").unwrap();
    for typed in ["gpt-reserve", "gpt-6-unlisted"] {
        assert!(!models(&rows).contains(&typed));
        let entry = catalog.entry(typed).unwrap();
        let caps = entry.capabilities(codex::baseline(Provider::OpenAI));
        assert_eq!(caps.context_length, Some(272_000));
    }
}

/// With no catalog the listing stands, less what is past its shutdown date:
/// a past date goes, a future date, today's date, and no date stay. The
/// listing's own order (newest first) holds.
#[test]
fn without_the_catalog_the_menu_is_the_listing_less_shut_down_models() {
    let rows = platform_rows(LISTING, Err("no route".into()), "2026-10-19").unwrap();
    assert_eq!(
        models(&rows),
        [
            "gpt-reserve",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.5",
            "text-embedding-4",
            "sora-2",
            "davinci-002",
        ]
    );
    // A later day takes the model whose date was today.
    let rows = platform_rows(LISTING, Err("no route".into()), "2026-10-20").unwrap();
    assert!(!models(&rows).contains(&"davinci-002"));
    assert!(models(&rows).contains(&"sora-2"));
}

/// A shutdown date in a spelling the parse does not read says nothing, so it
/// neither drops the entry nor fails the listing.
#[test]
fn an_unreadable_shutdown_date_says_nothing() {
    let body = r#"{"data":[
        {"id":"a","shutdown_date":1790000000},
        {"id":"b","shutdown_date":null},
        {"id":"c","shutdown_date":"soon"}
    ]}"#;
    let rows = listing::rows_not_shut_down(body, "2026-10-19").unwrap();
    assert_eq!(models(&rows).len(), 3);
}

/// A catalog that reads but names none of the listing's models (an empty one, or
/// one whose entries are all hidden or absent from the listing) would leave an
/// empty menu, so the menu falls back as it does with no catalog.
#[test]
fn a_catalog_naming_none_of_the_listing_falls_back_to_the_listing() {
    for catalog in [
        r#"{"models": []}"#,
        r#"{"models": [{"slug": "gpt-reserve", "visibility": "hide"}, {"slug": "gpt-elsewhere"}]}"#,
    ] {
        let rows =
            platform_rows(LISTING, Ok(codex::parse(catalog).unwrap()), "2026-10-19").unwrap();
        let fallback = platform_rows(LISTING, Err("no route".into()), "2026-10-19").unwrap();
        assert_eq!(rows, fallback);
        assert!(!models(&rows).contains(&"gpt-5.1-codex"));
    }
}
