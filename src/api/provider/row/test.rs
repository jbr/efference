use super::*;

#[test]
fn rfc3339_reads_utc_and_offsets() {
    assert_eq!(unix_from_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        unix_from_rfc3339("2026-02-04T00:00:00Z"),
        Some(1_770_163_200)
    );
    assert_eq!(
        unix_from_rfc3339("2026-02-04T01:00:00.250+01:00"),
        Some(1_770_163_200)
    );
    assert_eq!(unix_from_rfc3339("yesterday"), None);
}

#[test]
fn prices_are_per_million_and_negative_means_unknown() {
    let price = Price::from_per_token("0.000003", "0.000015").unwrap();
    assert_eq!((price.input, price.output), (3.0, 15.0));
    assert_eq!(Price::from_per_token("-1", "-1"), None);
    assert_eq!(Price::from_per_token("0.000001", "soon"), None);
}

#[test]
fn arrange_puts_newest_first_and_keeps_source_order_otherwise() {
    let dated = |id: &str, created: Option<i64>| ListedModel {
        created,
        ..ListedModel::new(id)
    };
    let ids = |rows: Vec<ListedModel>| rows.into_iter().map(|r| r.model).collect::<Vec<_>>();
    assert_eq!(
        ids(arrange(vec![
            dated("undated-a", None),
            dated("old", Some(1)),
            dated("undated-b", None),
            dated("new", Some(2)),
            dated("tie", Some(1)),
        ])),
        ["new", "old", "tie", "undated-a", "undated-b"]
    );
    assert_eq!(
        ids(arrange(vec![dated("z", None), dated("a", None)])),
        ["z", "a"]
    );
}

#[test]
fn absent_facts_do_not_serialize() {
    let row = ListedModel::new("m").named("m");
    assert_eq!(serde_json::to_string(&row).unwrap(), r#"{"model":"m"}"#);
}
