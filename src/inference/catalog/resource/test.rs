use super::*;
use crate::inference::api::{ListedModel, ModelKind, Price};
use crate::inference::catalog::{Catalog, Recent, assemble};
use crate::record::log::Provider;

async fn connection(name: &str, rows: Vec<ListedModel>, picks: &[Recent]) -> ConnectionCatalog {
    let catalog = Catalog::new(None);
    let snapshot = catalog
        .refresh_with(
            name,
            Provider::OpenRouter,
            super::super::CATALOG_TTL,
            || async { Ok(rows) },
        )
        .await;
    assemble(name, Provider::OpenRouter, snapshot, None, picks)
}

#[tokio::test]
async fn combined_filters_find_native_ids_names_and_provider_descriptions() {
    let mut row = ListedModel::new("vendor/new-id")
        .named("Friendly Name")
        .described(Some("Special résumé support"));
    row.context = Some(128_000);
    row.price = Some(Price {
        input: 0.125,
        output: 0.5,
    });
    row.reasoning = Some(true);
    row.images = Some(false);
    let connections = vec![
        connection("router", vec![row], &[]).await,
        connection("other", vec![ListedModel::new("vendor/new-id")], &[]).await,
    ];
    for q in ["NEW-ID", "Friendly+Name", "r%C3%A9sum%C3%A9"] {
        let query = Query::parse(&format!("{URI}?connection=router&role=main&q={q}")).unwrap();
        let selected = query.select(connections.clone()).unwrap();
        let text = query.render(&selected);
        assert!(text.contains("1 matches; showing 1"), "{text}");
        assert!(text.contains("- router/vendor/new-id — Friendly Name"));
        assert!(!text.contains("- other/"));
        assert!(text.contains("USD/1M input=0.125 output=0.5"));
        assert!(text.contains("context=128000;"));
        assert!(text.contains("reasoning=true; images=false"));
    }
}

#[tokio::test]
async fn roles_are_compatibility_not_editorial_and_unknown_prices_are_not_free() {
    let mut decisions = ListedModel::new("judge");
    decisions.kind = ModelKind::Decisions;
    let mut free = ListedModel::new("free");
    free.price = Some(Price {
        input: 0.0,
        output: 0.0,
    });
    let connections = vec![
        connection(
            "router",
            vec![decisions, ListedModel::new("unknown"), free],
            &[],
        )
        .await,
    ];
    for role in ["main", "triage", "spotter"] {
        let query = Query::parse(&format!("{URI}?role={role}")).unwrap();
        let text = query.render(&query.select(connections.clone()).unwrap());
        assert_eq!(text.contains("- router/judge;"), role == "spotter");
        assert!(text.contains("- router/unknown; price=unknown"));
        assert!(text.contains("- router/free; USD/1M input=0 output=0"));
    }
    let query = Query::parse(URI).unwrap();
    assert_eq!(query.role, ModelRole::Main);
    assert!(
        query.select(connections).unwrap()[0]
            .models
            .iter()
            .all(|m| m.row.kind.is_text())
    );
}

#[tokio::test]
async fn recents_are_first_ties_are_sorted_and_all_matches_are_pageable() {
    let picks = [Recent {
        model: "typed/not-listed".into(),
        rank: 1,
    }];
    let connections = vec![
        connection(
            "z",
            vec![ListedModel::new("z"), ListedModel::new("a")],
            &picks,
        )
        .await,
        connection("a", vec![ListedModel::new("b")], &[]).await,
    ];
    let query = Query::parse(&format!("{URI}?limit=2")).unwrap();
    let text = query.render(&query.select(connections.clone()).unwrap());
    assert!(text.contains("4 matches; showing 2"));
    assert!(text.contains("- z/typed/not-listed; recent #1; price=unknown\n- a/b;"));
    assert!(text.contains("set offset=2"));
    let query = Query::parse(&format!("{URI}?limit=2&offset=2")).unwrap();
    let text = query.render(&query.select(connections.clone()).unwrap());
    assert!(text.contains("- z/a; price=unknown\n- z/z; price=unknown"));
    assert!(!text.contains("More matches:"));
    let query = Query::parse(&format!("{URI}?offset={}", usize::MAX)).unwrap();
    assert!(
        query
            .render(&query.select(connections).unwrap())
            .contains("showing 0")
    );
}

#[tokio::test]
async fn empty_matches_and_unknown_connections_are_explicit() {
    let connections = vec![connection("router", vec![ListedModel::new("one")], &[]).await];
    let query = Query::parse(&format!("{URI}?q=no-such-model")).unwrap();
    assert!(
        query
            .render(&query.select(connections.clone()).unwrap())
            .contains("0 matches; showing 0")
    );
    let query = Query::parse(&format!("{URI}?connection=typo")).unwrap();
    assert!(
        query
            .select(connections)
            .unwrap_err()
            .contains("Connections: router")
    );
}

#[test]
fn malformed_or_ambiguous_queries_are_refused() {
    for suffix in [
        "?role=quick",
        "?limit=0",
        "?limit=101",
        "?offset=-1",
        "?q=x&q=y",
        "?wat=x",
        "?connection=",
        "?q=%GG",
        "?q=%FF",
        "?q=%",
        "?q",
        "#fragment",
        "/extra",
    ] {
        assert!(Query::parse(&format!("{URI}{suffix}")).is_err(), "{suffix}");
    }
    let query = Query::parse(&format!("{URI}?connection=a%2Fb&q=a%26b")).unwrap();
    assert_eq!(query.connection.as_deref(), Some("a/b"));
    assert_eq!(query.q.as_deref(), Some("a&b"));
}

#[tokio::test]
async fn narrow_reads_omit_help_and_explain_description_matches() {
    let connections = vec![
        connection(
            "router",
            vec![ListedModel::new("unrelated").described(Some(
                "A model mentioning Kimi K3 in its provider description",
            ))],
            &[],
        )
        .await,
    ];
    let query = Query::parse(&format!("{URI}?q=kimi+k3")).unwrap();
    let text = query.render(&query.select(connections).unwrap());
    assert!(!text.contains("Filters combine"), "{text}");
    assert!(text.contains("description match: …Kimi K3"), "{text}");
    assert!(text.contains("router: fresh;"), "{text}");
}

#[tokio::test]
async fn default_page_is_twenty_and_keeps_all_matches_accessible() {
    let connections = vec![
        connection(
            "router",
            (0..25)
                .map(|i| ListedModel::new(format!("model-{i:02}")))
                .collect(),
            &[],
        )
        .await,
    ];
    let query = Query::parse(URI).unwrap();
    let text = query.render(&query.select(connections.clone()).unwrap());
    assert!(text.contains("25 matches; showing 20"), "{text}");
    assert!(text.contains("set offset=20"), "{text}");
    assert!(text.contains("Filters combine"), "{text}");
    let query = Query::parse(&format!("{URI}?offset=20")).unwrap();
    let text = query.render(&query.select(connections).unwrap());
    assert!(text.contains("showing 5 at offset 20"), "{text}");
    assert!(!text.contains("Filters combine"), "{text}");
}

#[test]
fn catalogue_age_is_readable_and_unicode_excerpts_are_safe() {
    assert_eq!(age_label(12077), "3h 21m");
    assert_eq!(age_label(90000), "1d 1h");
    assert_eq!(
        match_excerpt("İ followed by Kimi K3", "kimi k3"),
        "…Kimi K3"
    );
}
