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
    entry(seq, Payload::Ambient(Notification::new("src", body, None)))
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
            shown_through: None,
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
    // No triage has shown it, so it is in the rider but not yet active.
    assert!(active_shown(&log).is_empty());
    let pending = rider(&log);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].seq, 1);
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

fn marked(seq: Seq, through: Seq) -> LogEntry {
    let mut e = triage(seq, Vec::new());
    if let Payload::Triage(t) = &mut e.payload {
        t.shown_through = Some(through);
    }
    e
}

#[test]
fn an_arrival_no_triage_has_shown_is_unshown_and_in_the_rider() {
    // An old-log triage (no mark) stands for its own seq; the later arrivals are unshown.
    let log = vec![
        ambient(0, "shown"),
        triage(1, Vec::new()),
        ambient(2, "arrived since"),
    ];
    assert_eq!(
        active_shown(&log).iter().map(|n| n.seq).collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        unshown(&log).iter().map(|n| n.seq).collect::<Vec<_>>(),
        vec![2]
    );
    assert!(has_unshown(&log));
    assert_eq!(
        rider(&log).iter().map(|n| n.seq).collect::<Vec<_>>(),
        vec![0, 2]
    );
}

#[test]
fn a_mark_below_an_entry_leaves_it_unshown_though_the_triage_follows_it() {
    // The notification arrived (seq 2) while the triage request was in flight; the triage
    // entry (seq 3) is written after it but records that it showed through seq 0.
    let log = vec![ambient(0, "old"), ambient(2, "in flight"), marked(3, 0)];
    assert_eq!(
        unshown(&log).iter().map(|n| n.seq).collect::<Vec<_>>(),
        vec![2]
    );
    // The next triage shows it.
    let mut log = log;
    log.push(marked(4, 2));
    assert!(unshown(&log).is_empty());
    assert!(!has_unshown(&log));
}

#[test]
fn a_triage_that_showed_no_rider_marks_zero_and_shows_nothing() {
    // Seq 0 is the boot `Preamble` in a real log, so a mark of 0 shows no notification.
    let log = vec![ambient(1, "waiting"), marked(2, 0)];
    assert_eq!(
        unshown(&log).iter().map(|n| n.seq).collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn a_pin_or_dismiss_resolves_an_arrival_no_triage_showed() {
    let log = vec![
        ambient(0, "pinned unseen"),
        ambient(1, "dismissed unseen"),
        ambient(2, "waiting"),
        marked(3, 0),
    ];
    let mut with_verdict = log.clone();
    if let Payload::Triage(t) = &mut with_verdict[3].payload {
        t.inbox = vec![(0, Disposition::Pin), (1, Disposition::Dismiss)];
    }
    assert!(unshown(&with_verdict).iter().all(|n| n.seq == 2));
    assert_eq!(rider(&with_verdict).len(), 1);
}

#[test]
fn an_arrival_during_a_request_is_not_counted_as_shown_by_it() {
    // Three triages; the last arrival (seq 1) landed during the first one's request, so
    // it was shown by the second and third only.
    let log = vec![
        ambient(0, "early"),
        ambient(1, "late"),
        marked(2, 0),
        marked(3, 1),
    ];
    assert_eq!(unmentioned_past_cap(&log), Vec::new());
    let mut log = log;
    log.push(marked(4, 1));
    assert_eq!(unmentioned_past_cap(&log), vec![(0, 3)]);
}

#[test]
fn a_batch_that_showed_no_rider_counts_for_nothing() {
    let mut log = vec![ambient(5, "left")];
    for i in 0..3 {
        let mut t = result_triage(7 + i * 2, 30 + i);
        if let Payload::Triage(t) = &mut t.payload {
            t.shown_through = Some(0);
        }
        log.push(assistant(6 + i * 2));
        log.push(t);
    }
    assert!(unmentioned_past_cap(&log).is_empty());
}

#[test]
fn the_rider_a_triage_was_shown_is_replayed_from_its_mark() {
    // seq 0 arrived before the prefix ended, 2 during the request, 3 after the mark.
    let log = vec![
        ambient(0, "before"),
        assistant(1),
        ambient(2, "first-shown slot"),
        {
            let mut t = result_triage(3, 9);
            if let Payload::Triage(t) = &mut t.payload {
                t.shown_through = Some(2);
                t.inbox = vec![(0, Disposition::Pin)];
            }
            t
        },
        ambient(4, "during"),
    ];
    let seqs: Vec<Seq> = rider_after(&log, 2).iter().map(|n| n.seq).collect();
    assert_eq!(seqs, vec![0, 2], "a pin by this triage stays in its rider");
}
