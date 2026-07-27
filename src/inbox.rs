//! The live ambient-notification inbox over the log (docs/inbox.md).
//!
//! The pending inbox is **event-sourced**: a notification a fork peek has shown is an
//! [`Payload::Ambient`] log entry, and it stays *active* until a [`Triage`] verdict
//! pins or dismisses its `seq`. [`active_shown`] folds the log to that active set —
//! the shown-but-unresolved notifications a gate re-shows ("left" / bumped, docs/inbox.md).
//! Not-yet-shown notifications live in the daemon's ephemeral queue (docs/control-channel.md)
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
mod test;
