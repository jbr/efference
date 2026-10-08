use super::*;

const MODELS: &str = include_str!("models.json");

#[test]
fn rows_carry_the_declared_facts_newest_first() {
    let rows = rows(MODELS).unwrap();
    assert_eq!(
        serde_json::to_string(&rows).unwrap(),
        concat!(
            r#"[{"model":"claude-opus-5.5","name":"Claude Opus 5.5","created":1788220800,"context":1000000,"images":true,"line":"opus","lifecycle":"active"},"#,
            r#"{"model":"claude-haiku-5","name":"Claude Haiku 5","created":1772323200,"context":200000,"images":true,"line":"haiku","lifecycle":"active"},"#,
            r#"{"model":"claude-text-only"}]"#
        )
    );
}

/// The default spotter is the newest `active` model on the `haiku` line, read from
/// the listing's `line` and `created_at` rather than parsed out of an id
/// (docs/model-catalog.md); a listing that names no lines answers nothing.
#[test]
fn the_newest_active_haiku_is_the_default_spotter() {
    let row = |id: &str, line: &str, lifecycle: &str, created: i64| ListedModel {
        created: Some(created),
        line: Some(line.into()),
        lifecycle: Some(lifecycle.into()),
        ..ListedModel::new(id)
    };
    let listed = [
        row("claude-sonnet-5-5", "sonnet", "active", 400),
        row("claude-haiku-5-5", "haiku", "active", 300),
        row("claude-haiku-4-5", "haiku", "active", 100),
        row("claude-haiku-6-preview", "haiku", "deprecated", 500),
    ];
    assert_eq!(
        Provider::Anthropic.newest_spotter_model(&listed).as_deref(),
        Some("claude-haiku-5-5")
    );
    assert_eq!(
        Provider::Anthropic
            .newest_spotter_model(&rows(MODELS).unwrap())
            .as_deref(),
        Some("claude-haiku-5")
    );
    assert_eq!(
        Provider::Anthropic.newest_spotter_model(&[ListedModel::new("claude-haiku-9")]),
        None
    );
    assert_eq!(Provider::OpenRouter.newest_spotter_model(&listed), None);
}
