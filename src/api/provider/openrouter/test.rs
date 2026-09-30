use super::*;

fn entries() -> Vec<Entry> {
    [
        ("openai/gpt-6", "OpenAI: GPT-6"),
        ("anthropic/claude-opus", "Anthropic: Claude Opus"),
        (
            "~anthropic/claude-opus-latest",
            "Anthropic: Claude Opus Latest",
        ),
        ("openai/gpt-6-mini", "OpenAI: GPT-6 Mini"),
        ("bare", "No author"),
    ]
    .into_iter()
    .map(|(id, name)| (id.to_string(), name.to_string()))
    .collect()
}

#[test]
fn authors_are_sorted_counted_and_fold_the_alias_marker() {
    assert_eq!(
        authors(&entries()),
        vec![
            ("anthropic".to_string(), "2 models".to_string()),
            ("openai".to_string(), "2 models".to_string()),
        ]
    );
}

#[test]
fn an_authors_models_keep_catalog_order_and_alias_ids() {
    let ids: Vec<String> = models_of(&entries(), "anthropic")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        ids,
        ["anthropic/claude-opus", "~anthropic/claude-opus-latest"]
    );
    assert!(models_of(&entries(), "nobody").is_empty());
}

#[test]
fn a_catalog_body_parses_to_ids_and_names() {
    let parsed = parse(r#"{"data":[{"id":"a/b","name":"A: B"}]}"#).unwrap();
    assert_eq!(parsed, vec![("a/b".to_string(), "A: B".to_string())]);
    assert!(parse("{}").is_err());
}

/// A trimmed real `GET /api/v1/models` body.
const CATALOG: &str = include_str!("catalog.json");

/// What the same route answered when the request carried `anthropic-version`:
/// Anthropic's listing shape, 20 rows, `display_name` instead of `name`.
const ANTHROPIC_SHAPED: &str = include_str!("anthropic_shaped.json");

#[test]
fn the_real_catalog_lists_many_authors_with_unprefixed_ids() {
    let catalog = parse(CATALOG).unwrap();
    assert_eq!(catalog.len(), 36);
    let by_author = authors(&catalog);
    assert!(by_author.len() > 10, "{by_author:?}");
    assert!(by_author.iter().all(|(a, _)| !a.starts_with('~')));
    let anthropic = models_of(&catalog, "anthropic");
    assert!(
        anthropic
            .iter()
            .any(|(id, _)| id == "anthropic/claude-opus-5.5")
    );
    assert!(
        anthropic
            .iter()
            .any(|(id, _)| id == "~anthropic/claude-opus-latest")
    );
    assert!(anthropic.iter().all(|(id, _)| !id.contains("[1m]")));
}

#[test]
fn an_anthropic_shaped_body_is_an_error_not_a_listing() {
    assert!(parse(ANTHROPIC_SHAPED).is_err());
}
