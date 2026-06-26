//! The live ambient-notification inbox over the log (DESIGN §6).
//!
//! The pending inbox is **event-sourced**: a notification a fork peek has shown is an
//! [`Payload::Ambient`] log entry, and it stays *active* until a [`Triage`] verdict
//! pins or dismisses its `seq`. [`active_shown`] folds the log to that active set —
//! the shown-but-unresolved notifications a gate re-shows ("left" / bumped, DESIGN §6).
//! Not-yet-shown notifications live in the daemon's ephemeral queue (DESIGN §10.1)
//! until a gate logs them as `Ambient` (first-show); the loop concatenates the two.
//!
//! [`Triage`]: crate::log::Triage

use std::collections::HashSet;

use crate::log::{LogEntry, Payload, Seq};

/// A notification surfaced to the model in a fork peek, addressed by its arrival
/// `seq` — its durable id, the `[seq]` handle the `retain` inbox verdict references.
#[derive(Clone, Debug)]
pub struct ShownNotification {
    pub seq: Seq,
    pub source: String,
    pub body: String,
}

/// Fold the log to the **active** shown notifications: every `Ambient` entry whose
/// `seq` no [`Triage`](crate::log::Triage) verdict has yet pinned or dismissed.
/// Pinned/dismissed seqs are resolved (gone); unmentioned ("left") seqs stay active,
/// so a later gate re-shows them. In log order.
pub fn active_shown(log: &[LogEntry]) -> Vec<ShownNotification> {
    // Every seq any Triage dispositioned (pin or dismiss) is resolved — off the inbox.
    let resolved: HashSet<Seq> = log
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Triage(t) => Some(t.inbox.iter().map(|(seq, _)| *seq)),
            _ => None,
        })
        .flatten()
        .collect();
    log.iter()
        .filter_map(|e| match &e.payload {
            Payload::Ambient(n) if !resolved.contains(&e.seq) => Some(ShownNotification {
                seq: e.seq,
                source: n.source.clone(),
                body: n.body.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Disposition, MarkKind, Notification, Timestamp, Triage};

    fn entry(seq: Seq, payload: Payload) -> LogEntry {
        LogEntry { seq, turn: 0, at: Timestamp(seq), payload }
    }
    fn ambient(seq: Seq, body: &str) -> LogEntry {
        entry(seq, Payload::Ambient(Notification {
            source: "src".into(),
            body: body.into(),
            slot: None,
        }))
    }
    fn triage(seq: Seq, inbox: Vec<(Seq, Disposition)>) -> LogEntry {
        entry(seq, Payload::Triage(Triage {
            for_result: 0,
            origin: crate::log::Origin::Model,
            raw_keep: "all".into(),
            inbox,
        }))
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
            entry(0, Payload::Mark(crate::log::Mark {
                kind: MarkKind::Boundary { upto: 0 },
                note: "x".into(),
            })),
            ambient(1, "still pending"),
        ];
        let active = active_shown(&log);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].seq, 1);
    }
}
