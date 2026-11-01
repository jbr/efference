//! A notification no triage verdict mentions is dismissed by the harness after a bounded
//! number of shows, and the dismissal is a log entry (docs/inbox.md).
use super::*;
use crate::inbox::MAX_UNMENTIONED_SHOWS;
use crate::record::log::Telemetry;

fn idle_triage() -> Payload {
    Payload::Triage(Triage::new(None, String::new()))
}

#[test]
fn an_unmentioned_notification_expires_with_a_log_entry_after_the_cap() {
    let (shared, _rx) = Session::in_memory();
    let seq = shared
        .append(
            0,
            Payload::Ambient(Notification::new("ci", "42 passed", None)),
        )
        .unwrap();
    for shown in 1..=MAX_UNMENTIONED_SHOWS {
        shared.append(0, idle_triage()).unwrap();
        expire_unmentioned(&shared, 0).unwrap();
        let expired = shared
            .snapshot()
            .iter()
            .filter(|e| {
                matches!(
                    e.payload,
                    Payload::Telemetry(Telemetry::InboxExpired { .. })
                )
            })
            .count();
        assert_eq!(expired, usize::from(shown == MAX_UNMENTIONED_SHOWS));
    }
    let log = shared.snapshot();
    assert!(
        log.iter().any(|e| matches!(
            e.payload,
            Payload::Telemetry(Telemetry::InboxExpired { seq: s, shows })
                if s == seq && shows == MAX_UNMENTIONED_SHOWS
        )),
        "the breadcrumb names the notification and the show count"
    );
    assert!(
        crate::inbox::active_shown(&log).is_empty(),
        "it no longer resurfaces"
    );
    // Running the fold again appends nothing: the entry resolved it.
    expire_unmentioned(&shared, 0).unwrap();
    assert_eq!(shared.snapshot().len(), log.len());
}
