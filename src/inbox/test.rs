
use super::*;
use crate::log::{Disposition, MarkKind, Notification, Timestamp, Triage};

fn entry(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(seq),
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
    // A mark and a fresh ambient with no verdict yet: the ambient is active.
    let log = vec![
        entry(
            0,
            Payload::Mark(crate::log::Mark {
                kind: MarkKind::Boundary { upto: 0 },
                note: "x".into(),
            }),
        ),
        ambient(1, "still pending"),
    ];
    let active = active_shown(&log);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].seq, 1);
}
