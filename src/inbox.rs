//! The live ambient-notification inbox over the log (docs/inbox.md).
//!
//! The pending inbox is **event-sourced**: a notification a triage has shown is an
//! [`Payload::Ambient`] log entry, and it stays *active* until a [`Triage`] verdict
//! pins or dismisses its `seq`. [`active_shown`] folds the log to that active set —
//! the shown-but-unresolved notifications a triage re-shows ("left" / bumped, docs/inbox.md).
//! Not-yet-shown notifications live in the daemon's ephemeral queue (docs/control-channel.md)
//! until a triage logs them as `Ambient` (first-show); the loop concatenates the two.
//!
//! [`Triage`]: crate::log::Triage

use std::collections::{BTreeMap, HashSet};

use crate::log::{LogEntry, Payload, Seq, Telemetry};

/// How many triages display a notification that none of their verdicts mention before the
/// harness dismisses it (docs/inbox.md). A notification left unmentioned is shown again
/// at the next triage, so without a bound it is re-shown for as long as the session runs.
pub const MAX_UNMENTIONED_SHOWS: usize = 3;

/// A notification surfaced to the model in a triage, addressed by its arrival
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
/// so a later triage re-shows them. In log order.
pub fn active_shown(log: &[LogEntry]) -> Vec<ShownNotification> {
    let resolved = resolved(log);
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

/// Every seq a `Triage` verdict dispositioned (pin or dismiss) or the harness expired
/// ([`Telemetry::InboxExpired`]) — off the inbox.
fn resolved(log: &[LogEntry]) -> HashSet<Seq> {
    let mut out = HashSet::new();
    for e in log {
        match &e.payload {
            Payload::Triage(t) => out.extend(t.inbox.iter().map(|(seq, _)| *seq)),
            Payload::Telemetry(Telemetry::InboxExpired { seq, .. }) => {
                out.insert(*seq);
            }
            _ => {}
        }
    }
    out
}

/// The active notifications that [`MAX_UNMENTIONED_SHOWS`] triages have displayed without
/// any verdict mentioning them, each with its show count, in log order.
///
/// A show is one triage: a `Triage` entry with no result (the idle triage), or a run of
/// result-carrying `Triage` entries with no `Assistant` or `User` entry between them, which
/// is one completion's batch (its verdict rides the last of them, docs/triage.md). Every
/// triage displays every notification active when it ran, so the count needs no record of
/// what a triage was shown.
pub fn unmentioned_past_cap(log: &[LogEntry]) -> Vec<(Seq, usize)> {
    let resolved = resolved(log);
    let mut shows: BTreeMap<Seq, usize> = BTreeMap::new();
    // The notifications active when the open batch's first `Triage` was written.
    let mut batch: Option<Vec<Seq>> = None;
    let flush = |batch: &mut Option<Vec<Seq>>, shows: &mut BTreeMap<Seq, usize>| {
        for seq in batch.take().into_iter().flatten() {
            *shows.entry(seq).or_default() += 1;
        }
    };
    for e in log {
        match &e.payload {
            Payload::Ambient(_) if !resolved.contains(&e.seq) => {
                shows.entry(e.seq).or_default();
            }
            Payload::Triage(t) if t.for_result.is_none() => {
                flush(&mut batch, &mut shows);
                batch = Some(shows.keys().copied().collect());
                flush(&mut batch, &mut shows);
            }
            Payload::Triage(_) => {
                batch.get_or_insert_with(|| shows.keys().copied().collect());
            }
            Payload::Assistant { .. } | Payload::User(_) => flush(&mut batch, &mut shows),
            _ => {}
        }
    }
    flush(&mut batch, &mut shows);
    shows
        .into_iter()
        .filter(|&(_, n)| n >= MAX_UNMENTIONED_SHOWS)
        .collect()
}

#[cfg(test)]
mod test;
