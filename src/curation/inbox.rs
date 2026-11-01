//! The live ambient-notification inbox over the log (docs/inbox.md).
//!
//! The inbox is **event-sourced**: a notification without a slot is an [`Payload::Ambient`]
//! log entry from the moment it arrives, and it stays *active* until a [`Triage`] verdict
//! pins or dismisses its `seq`. Whether a triage has shown it is a second fold over the
//! same log: each `Triage` records the last arrival its rider showed
//! ([`Triage::shown_through`]), and an arrival above every such mark is **unshown**.
//! [`active_shown`] is the shown-but-unresolved set a triage re-shows ("left" / bumped),
//! [`unshown`] the arrived-but-unshown set, and [`rider`] their union, which is what a
//! rider holds. Slot notifications (a progress line a later one supersedes) wait in the
//! daemon's queue and are logged when a triage first shows them (docs/control-channel.md).
//!
//! [`Triage`]: crate::record::log::Triage
//! [`Triage::shown_through`]: crate::record::log::Triage::shown_through

use std::collections::{BTreeMap, HashSet};

use crate::record::log::{LogEntry, Payload, Seq, Telemetry};

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
    /// Must-wake (docs/inbox.md), as recorded on the arrival entry.
    pub wake: bool,
}

impl ShownNotification {
    pub fn new(seq: Seq, source: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            seq,
            source: source.into(),
            body: body.into(),
            wake: false,
        }
    }

    fn of(e: &LogEntry) -> Option<Self> {
        match &e.payload {
            Payload::Ambient(n) => Some(Self {
                seq: e.seq,
                source: n.source.clone(),
                body: n.body.clone(),
                wake: n.wake,
            }),
            _ => None,
        }
    }
}

/// The highest arrival `seq` any triage's rider showed (docs/inbox.md); a `Triage` that
/// records no mark stands for its own `seq`. `None` when no triage has run.
pub fn shown_through(log: &[LogEntry]) -> Option<Seq> {
    log.iter().filter_map(triage_mark).max()
}

/// The arrival mark of one `Triage` entry: what it recorded, else its own `seq`.
pub fn triage_mark(e: &LogEntry) -> Option<Seq> {
    match &e.payload {
        Payload::Triage(t) => Some(t.shown_through.unwrap_or(e.seq)),
        _ => None,
    }
}

/// The notifications no verdict has yet pinned or dismissed, in log order, each with
/// whether a triage's rider has shown it.
fn unresolved(log: &[LogEntry]) -> impl Iterator<Item = (ShownNotification, bool)> + '_ {
    let resolved = resolved(log);
    let through = shown_through(log);
    log.iter().filter_map(move |e| {
        let n = ShownNotification::of(e)?;
        (!resolved.contains(&e.seq)).then(|| {
            let shown = through.is_some_and(|t| e.seq <= t);
            (n, shown)
        })
    })
}

/// Fold the log to the **active** shown notifications: every `Ambient` entry a triage's
/// rider has shown and no [`Triage`](crate::record::log::Triage) verdict has yet pinned or
/// dismissed. Pinned/dismissed seqs are resolved (gone); unmentioned ("left") seqs stay
/// active, so a later triage re-shows them. In log order.
pub fn active_shown(log: &[LogEntry]) -> Vec<ShownNotification> {
    unresolved(log)
        .filter_map(|(n, shown)| shown.then_some(n))
        .collect()
}

/// The arrived notifications no triage's rider has shown yet, in log order: what the idle
/// wake looks for, and what a restart restores (docs/resumption.md).
pub fn unshown(log: &[LogEntry]) -> Vec<ShownNotification> {
    unresolved(log)
        .filter_map(|(n, shown)| (!shown).then_some(n))
        .collect()
}

/// Whether [`unshown`] would be non-empty, without copying the notifications.
pub fn has_unshown(log: &[LogEntry]) -> bool {
    unresolved(log).any(|(_, shown)| !shown)
}

/// The inbox a triage opening now shows: every unresolved arrival, shown before or not,
/// in log order. The rider shows all of them or none (a result triage the shape rule
/// decided without the model shows nothing, and records so), which is what lets a single
/// mark stand for what it showed.
pub fn rider(log: &[LogEntry]) -> Vec<ShownNotification> {
    unresolved(log).map(|(n, _)| n).collect()
}

/// The rider the triage that followed `entries[..prefix_end]` was shown, for replaying it:
/// the notifications arrived by that triage's mark and not resolved before it. The triage's
/// own entries are those from `prefix_end` to the next `Assistant`; a verdict among them
/// may have resolved a notification, which stays in the rider it was shown in.
pub fn rider_after(entries: &[LogEntry], prefix_end: usize) -> Vec<ShownNotification> {
    let batch_end = entries[prefix_end..]
        .iter()
        .position(|e| matches!(&e.payload, Payload::Assistant { .. }))
        .map_or(entries.len(), |i| prefix_end + i);
    let Some(through) = entries[prefix_end..batch_end]
        .iter()
        .filter_map(triage_mark)
        .max()
    else {
        return Vec::new();
    };
    let resolved = resolved(&entries[..prefix_end]);
    entries[..batch_end]
        .iter()
        .filter(|e| e.seq <= through && !resolved.contains(&e.seq))
        .filter_map(ShownNotification::of)
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
/// is one completion's batch (its verdict rides the last of them, docs/triage.md). A
/// triage displays the notifications arrived up to its [`Triage::shown_through`] mark, so
/// one that arrived while its request was in flight counts from the next triage, and a
/// triage that showed no rider counts for none.
///
/// [`Triage::shown_through`]: crate::record::log::Triage::shown_through
pub fn unmentioned_past_cap(log: &[LogEntry]) -> Vec<(Seq, usize)> {
    let resolved = resolved(log);
    let mut shows: BTreeMap<Seq, usize> = BTreeMap::new();
    // The highest arrival the open batch's `Triage` entries showed.
    let mut batch: Option<Seq> = None;
    let flush = |batch: &mut Option<Seq>, shows: &mut BTreeMap<Seq, usize>| {
        if let Some(mark) = batch.take() {
            for (_, n) in shows.range_mut(..=mark) {
                *n += 1;
            }
        }
    };
    for e in log {
        match &e.payload {
            Payload::Ambient(_) if !resolved.contains(&e.seq) => {
                shows.entry(e.seq).or_default();
            }
            Payload::Triage(t) if t.for_result.is_none() => {
                flush(&mut batch, &mut shows);
                batch = triage_mark(e);
                flush(&mut batch, &mut shows);
            }
            Payload::Triage(_) => {
                batch = batch.max(triage_mark(e));
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
