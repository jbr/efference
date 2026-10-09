//! Tests for `attention.rs`.

use super::*;

fn report(client: &str, viewing: Option<&str>, active: bool) -> AttentionReport {
    AttentionReport {
        client: client.into(),
        viewing: viewing.map(str::to_string),
        active,
    }
}

/// A report is `{client, viewing, active}`; `viewing` may be null, and a body
/// that is not that shape is refused with a message rather than read leniently.
#[test]
fn a_report_parses_and_a_malformed_one_is_rejected() {
    let parsed = AttentionReport::parse(r#"{"client":"c1","viewing":"hardy-hawk","active":true}"#)
        .expect("a well-formed report");
    assert_eq!(parsed, report("c1", Some("hardy-hawk"), true));
    let list = AttentionReport::parse(r#"{"client":"c1","viewing":null,"active":false}"#).unwrap();
    assert_eq!(list.viewing, None);

    for bad in [
        "",
        "not json",
        "{}",
        r#"{"client":"c1","viewing":null}"#,
        r#"{"client":"","viewing":null,"active":true}"#,
        r#"{"client":"c1","viewing":"","active":true}"#,
        r#"{"client":"c1","viewing":null,"active":"yes"}"#,
        r#"{"client":"c1","viewing":null,"active":true,"extra":1}"#,
    ] {
        assert!(AttentionReport::parse(bad).is_err(), "{bad:?}");
    }
    let long = "x".repeat(MAX_ID_LEN + 1);
    let body = serde_json::json!({"client": long, "viewing": null, "active": true});
    assert!(AttentionReport::parse(&body.to_string()).is_err());
}

/// Someone is attentive while any unlapsed client's last report was active.
#[test]
fn any_active_unlapsed_client_is_attentive() {
    let attention = Attention::default();
    let t0 = Instant::now();
    assert!(!attention.attentive_at(t0), "no reports, no attention");

    attention.report_at(&report("a", None, false), t0);
    assert!(!attention.attentive_at(t0), "an inactive client is not");

    attention.report_at(&report("b", Some("s"), true), t0);
    assert!(attention.attentive_at(t0));

    // The latest report from a client replaces its earlier one.
    attention.report_at(&report("b", Some("s"), false), t0 + Duration::from_secs(1));
    assert!(!attention.attentive_at(t0 + Duration::from_secs(1)));
}

/// A client whose reports stop lapses after [`LAPSE`]; a fresh report revives it.
#[test]
fn a_client_lapses_when_its_reports_stop() {
    let attention = Attention::default();
    let t0 = Instant::now();
    attention.report_at(&report("a", None, true), t0);
    assert!(attention.attentive_at(t0 + LAPSE - Duration::from_secs(1)));
    assert!(!attention.attentive_at(t0 + LAPSE));

    attention.report_at(&report("a", None, true), t0 + LAPSE);
    assert!(attention.attentive_at(t0 + LAPSE + Duration::from_secs(1)));
}

/// Lapsed clients are dropped when the next report arrives, so the table does not
/// grow with page loads.
#[test]
fn lapsed_clients_are_pruned_on_the_next_report() {
    let attention = Attention::default();
    let t0 = Instant::now();
    for n in 0..5 {
        attention.report_at(&report(&format!("c{n}"), None, true), t0);
    }
    attention.report_at(&report("late", None, true), t0 + LAPSE);
    assert_eq!(attention.clients.lock().unwrap().len(), 1);
}
