use super::*;

fn at(t0: Instant, secs: u64) -> Instant {
    t0 + Duration::from_secs(secs)
}

fn attender(t0: Instant) -> Attender {
    Attender::new("c1".into(), Some("s1".into()), t0)
}

#[test]
fn starts_focused_and_active_and_reports_once() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    let report = a.due(t0).expect("first report");
    assert_eq!(
        report,
        AttentionReport {
            client: "c1".into(),
            session: Some("s1".into()),
            active: true
        }
    );
    assert_eq!(a.due(at(t0, 1)), None);
}

#[test]
fn focus_loss_reports_inactive_at_once_and_focus_gain_reactivates() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    a.due(t0);
    a.focus(false);
    assert!(!a.due(at(t0, 2)).unwrap().active);
    assert_eq!(a.due(at(t0, 3)), None);
    // Focus back inside the input window: active again.
    a.focus(true);
    assert!(a.due(at(t0, 4)).unwrap().active);
}

#[test]
fn input_after_silence_reactivates() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    a.due(t0);
    // Heartbeats continue until the window lapses, then one inactive report.
    let mut now = t0;
    let mut last = None;
    while now < at(t0, 5 * 60 + 30) {
        now += Duration::from_secs(5);
        if let Some(r) = a.due(now) {
            last = Some((r.active, now));
        }
    }
    assert_eq!(last.map(|(active, _)| active), Some(false));
    a.input(now);
    assert!(a.due(now).unwrap().active);
}

#[test]
fn heartbeat_every_twenty_seconds_while_active_only() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    a.due(t0);
    assert_eq!(a.due(at(t0, 19)), None);
    assert!(a.due(at(t0, 20)).unwrap().active);
    assert_eq!(a.due(at(t0, 39)), None);
    assert!(a.due(at(t0, 40)).is_some());
    a.focus(false);
    a.due(at(t0, 41));
    assert_eq!(a.due(at(t0, 120)), None, "no heartbeat while inactive");
}

#[test]
fn an_unfocused_start_with_no_input_says_nothing() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    a.focus(false);
    assert_eq!(a.due(t0), None);
}

#[test]
fn leaving_reports_inactive_once_after_an_active_report() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    assert_eq!(a.leaving(), None, "nothing active was said");
    a.due(t0);
    let report = a.leaving().unwrap();
    assert!(!report.active);
    assert_eq!(report.session.as_deref(), Some("s1"));
    assert_eq!(a.leaving(), None);
    assert_eq!(a.due(at(t0, 60)), None);
}

#[test]
fn stop_silences_everything() {
    let t0 = Instant::now();
    let mut a = attender(t0);
    a.stop();
    assert_eq!(a.due(t0), None);
    assert_eq!(a.leaving(), None);
}

#[test]
fn the_wire_shape_matches_the_host_route() {
    let r = AttentionReport {
        client: "c".into(),
        session: None,
        active: false,
    };
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"client":"c","session":null,"active":false}"#
    );
}
