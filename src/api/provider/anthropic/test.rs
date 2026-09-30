use super::*;

const MODELS: &str = include_str!("models.json");

#[test]
fn rows_carry_the_declared_facts_newest_first() {
    let rows = rows(MODELS).unwrap();
    assert_eq!(
        serde_json::to_string(&rows).unwrap(),
        concat!(
            r#"[{"model":"claude-opus-5.5","name":"Claude Opus 5.5","created":1788220800,"context":1000000,"images":true},"#,
            r#"{"model":"claude-haiku-5","name":"Claude Haiku 5","created":1772323200,"context":200000,"images":true},"#,
            r#"{"model":"claude-text-only"}]"#
        )
    );
}
