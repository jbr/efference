use super::*;

/// A trimmed real `GET /api/v1/models` body.
const CATALOG: &str = include_str!("catalog.json");

/// What the same route answered when the request carried `anthropic-version`:
/// Anthropic's listing shape, 20 rows, `display_name` instead of `name`.
const ANTHROPIC_SHAPED: &str = include_str!("anthropic_shaped.json");

fn row<'a>(rows: &'a [ListedModel], id: &str) -> &'a ListedModel {
    rows.iter().find(|row| row.model == id).unwrap()
}

#[test]
fn authors_fold_the_alias_marker() {
    assert_eq!(author_of("anthropic/claude"), Some("anthropic"));
    assert_eq!(author_of("~anthropic/claude-latest"), Some("anthropic"));
    assert_eq!(author_of("bare"), None);
}

#[test]
fn names_lose_the_author_prefix_only_when_there_is_one() {
    assert_eq!(strip_author("Z.ai: GLM 5.3 Flash"), "GLM 5.3 Flash");
    assert_eq!(strip_author("Space Bunny Alpha"), "Space Bunny Alpha");
    assert_eq!(strip_author("A: B: C"), "B: C");
}

#[test]
fn a_full_row_carries_every_declared_fact() {
    let rows = rows(CATALOG).unwrap();
    assert_eq!(
        serde_json::to_string(row(&rows, "z-ai/glm-5.3-prime")).unwrap(),
        r#"{"model":"z-ai/glm-5.3-prime","group":"z-ai","name":"GLM 5.3 Prime","description":"GLM-5.3-Prime is the high-speed variant of Z.ai's GLM-5.3, inheriting its full capabilities while delivering 1.5–2× the output throughput through inference acceleration. It supports text input and output with a 1M-token...","created":1790199651,"context":1000000,"price":{"input":2.8,"output":8.8},"reasoning":true,"images":false}"#
    );
    let aliased = row(&rows, "~anthropic/claude-opus-latest");
    assert_eq!(aliased.group.as_deref(), Some("anthropic"));
    assert_eq!(aliased.name.as_deref(), Some("Claude Opus Latest"));
    assert_eq!(row(&rows, "openai/gpt-6.1-sol-pro").images, Some(true));
    assert_eq!(row(&rows, "unbiased/pareto").reasoning, Some(false));
    assert_eq!(
        row(&rows, "stealth/space-bunny-alpha").price.unwrap().input,
        0.0
    );
}

#[test]
fn rows_without_tools_and_batch_variants_are_dropped_and_free_stays() {
    let rows = rows(CATALOG).unwrap();
    assert_eq!(rows.len(), 33);
    assert!(rows.iter().all(|r| r.model != "typesafe/jev-router"));
    assert!(rows.iter().all(|r| !r.model.ends_with(":batch")));
    let free = rows_of(
        r#"{"data":[{"id":"a/b:free","name":"A: B (free)","supported_parameters":["tools"]}]}"#,
    );
    assert_eq!(free.len(), 1);
}

fn rows_of(body: &str) -> Vec<ListedModel> {
    rows(body).unwrap()
}

#[test]
fn rows_run_newest_first() {
    let rows = rows(CATALOG).unwrap();
    let created: Vec<i64> = rows.iter().map(|r| r.created.unwrap()).collect();
    assert!(created.windows(2).all(|pair| pair[0] >= pair[1]));
    assert_eq!(rows[0].model, "openai/gpt-6.1-sol-pro");
}

#[test]
fn a_bare_entry_has_only_what_it_declares() {
    let rows = rows_of(r#"{"data":[{"id":"bare","name":""}]}"#);
    assert_eq!(rows, vec![ListedModel::new("bare")]);
}

#[test]
fn an_anthropic_shaped_body_is_an_error_not_a_listing() {
    assert!(rows(ANTHROPIC_SHAPED).is_err());
    assert!(rows("{}").is_err());
}

#[test]
fn model_description_is_provider_prose() {
    let rows = rows(r#"{"data":[{"id":"a/b","name":"A: B","description":"Provider **prose**.\nSecond paragraph."}]}"#).unwrap();
    assert_eq!(
        rows[0].description.as_deref(),
        Some("Provider **prose**.\nSecond paragraph.")
    );
}
