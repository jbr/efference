use super::*;

const OPENAI: &str = include_str!("openai.json");
const DESCRIBED: &str = include_str!("described.json");

fn json(rows: &[ListedModel]) -> String {
    serde_json::to_string(rows).unwrap()
}

#[test]
fn a_bare_openai_listing_keeps_id_and_date_newest_first() {
    assert_eq!(
        json(&rows(OPENAI).unwrap()),
        r#"[{"model":"gpt-6-sol","created":1790500000},{"model":"gpt-6-luna","created":1790000000},{"model":"text-embedding-4","created":1780000000}]"#
    );
}

#[test]
fn a_described_listing_reads_each_spelling_it_finds() {
    assert_eq!(
        json(&rows(DESCRIBED).unwrap()),
        r#"[{"model":"acme/large","name":"Large","created":1790000100,"context":200000,"images":true},{"model":"acme/small","name":"Small","context":32000},{"model":"acme/plain"}]"#
    );
}

#[test]
fn a_body_that_is_not_a_listing_is_an_error() {
    assert!(rows("42").is_err());
}

#[test]
fn a_zero_creation_time_is_no_date() {
    let body = r#"{"data":[{"id":"a","created":0},{"id":"b","created":1790000000}]}"#;
    assert_eq!(
        json(&rows(body).unwrap()),
        r#"[{"model":"b","created":1790000000},{"model":"a"}]"#
    );
}
