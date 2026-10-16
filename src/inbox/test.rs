use super::*;
use crate::log::{Disposition, Notification, Timestamp, Triage};

fn entry(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(seq),
        submission_id: None,
        source: None,
        payload,
    }
}
fn ambient(seq: Seq, body: &str) -> LogEntry {
    entry(
        seq,
        Payload::Ambient(Notification {
            source: "src".into(),
            body: body.into(),
            slot: None,
        }),
    )
}
fn triage(seq: Seq, inbox: Vec<(Seq, Disposition)>) -> LogEntry {
    entry(
        seq,
        Payload::Triage(Triage {
            for_result: None,
            origin: crate::log::Origin::Model,
            raw_keep: "all".into(),
            inbox,
            surfaced: Vec::new(),
            retries: Vec::new(),
            thinking: Vec::new(),
        }),
    )
}

#[test]
fn active_shown_resolves_pinned_and_dismissed_keeps_left() {
    // Three shown notifications; a later verdict pins one, dismisses one, leaves one.
    let log = vec![
        ambient(0, "pin me"),
        ambient(1, "dismiss me"),
        ambient(2, "leave me"),
        triage(3, vec![(0, Disposition::Pin), (1, Disposition::Dismiss)]),
    ];
    let active = active_shown(&log);
    // Only the un-dispositioned ("left") one remains active.
    assert_eq!(active.len(), 1, "{active:?}");
    assert_eq!(active[0].seq, 2);
    assert_eq!(active[0].body, "leave me");
}

#[test]
fn active_shown_ignores_non_ambient_and_unresolved_stay() {
    // A summary and a fresh ambient with no verdict yet: the ambient is active.
    let log = vec![
        entry(
            0,
            Payload::Summary(crate::log::Summary::consolidation(0, 0, "x")),
        ),
        ambient(1, "still pending"),
    ];
    let active = active_shown(&log);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].seq, 1);
}

fn assistant(seq: Seq) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: crate::log::AssistantOutput::empty(crate::api::WireDialect::Messages),
            upstream_failure: None,
        },
    )
}
fn result_triage(seq: Seq, for_result: Seq) -> LogEntry {
    let mut e = triage(seq, Vec::new());
    if let Payload::Triage(t) = &mut e.payload {
        t.for_result = Some(for_result);
    }
    e
}
fn expired(seq: Seq, of: Seq) -> LogEntry {
    entry(
        seq,
        Payload::Telemetry(Telemetry::InboxExpired { seq: of, shows: 3 }),
    )
}

#[test]
fn unmentioned_counts_one_show_per_batch_not_per_triage_entry() {
    // Two completions, the first a two-result batch (two `Triage` entries, one show).
    let log = vec![
        ambient(0, "left alone"),
        assistant(1),
        result_triage(2, 10),
        result_triage(3, 11),
        assistant(4),
        result_triage(5, 12),
    ];
    assert!(unmentioned_past_cap(&log).is_empty());
    let mut log = log;
    log.push(assistant(6));
    log.push(result_triage(7, 13));
    assert_eq!(unmentioned_past_cap(&log), vec![(0, 3)]);
}

#[test]
fn unmentioned_counts_idle_triages_and_skips_mentioned_and_expired() {
    let mut log = vec![
        ambient(0, "left"),
        ambient(1, "dismissed"),
        ambient(2, "expired"),
        triage(3, vec![(1, Disposition::Dismiss)]),
        triage(4, Vec::new()),
        triage(5, Vec::new()),
    ];
    assert_eq!(unmentioned_past_cap(&log), vec![(0, 3), (2, 3)]);
    log.push(expired(6, 2));
    assert_eq!(unmentioned_past_cap(&log), vec![(0, 3)]);
    assert_eq!(
        active_shown(&log).iter().map(|n| n.seq).collect::<Vec<_>>(),
        vec![0]
    );
}

#[test]
fn a_notification_first_shown_later_counts_only_from_its_arrival() {
    let log = vec![
        ambient(0, "old"),
        triage(1, Vec::new()),
        triage(2, Vec::new()),
        ambient(3, "new"),
        triage(4, Vec::new()),
    ];
    assert_eq!(unmentioned_past_cap(&log), vec![(0, 3)]);
}
