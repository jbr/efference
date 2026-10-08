use super::*;

// The complete listing returned on 2026-10-01.
const CATALOG: &str = include_str!("catalog.json");

#[test]
fn names_are_model_ids_and_release_dates_order_the_rows() {
    let rows = rows(CATALOG).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].model, "jev-preview");
    assert_eq!(rows[1].model, "jev-latest");
    assert!(rows[0].created.unwrap() > rows[1].created.unwrap());
    assert_eq!(rows[0].name, None);
    assert_eq!(
        rows[0].description.as_deref(),
        Some("A preview version of `jev-latest`: should be better in most ways")
    );
    assert_eq!(
        rows[1].description.as_deref(),
        Some("The latest iteration of TypeSafe's System One Model: Jev")
    );
}

#[test]
fn missing_facts_stay_absent_and_invalid_dates_do_not_drop_models() {
    let rows = rows(r#"{"models":[{"name":"bare"},{"name":"other","release_date":"unknown","description":"  "}]}"#).unwrap();
    let decision = |id| ListedModel {
        kind: ModelKind::Decisions,
        ..ListedModel::new(id)
    };
    assert_eq!(rows, vec![decision("bare"), decision("other")]);
}

/// Everything this provider lists answers questions rather than producing turns.
#[test]
fn every_listed_model_is_a_decision_model() {
    assert!(
        rows(CATALOG)
            .unwrap()
            .iter()
            .all(|row| row.kind == ModelKind::Decisions)
    );
}

#[test]
fn a_body_that_is_not_a_listing_is_refused() {
    for body in ["{}", r#"{"models":[{"description":"no id"}]}"#, "42"] {
        assert!(rows(body).is_err());
    }
}
