//! How much of what triage saw survives onto the wire (docs/triage.md).
//!
//! One rule, one place. What triage saw and what it kept are counted together,
//! against the same [`selection`] the transcript renders in its gutter — so these
//! are the numbers the gutter shows rather than a second, drifting definition of
//! "kept".
//!
//! A result's verdict is not always on the result: a result captured before its
//! triage carries [`WireState::Pending`] until the [`Payload::ResultVerdict`] entry
//! that decides it (docs/triage.md). The fold counts a result from whichever
//! of the two carries a final wire — its own entry (a result logged before capture
//! existed, or a fixture) or the verdict naming it — which is the same resolution
//! [`project`](crate::project::project) makes, so the ratio counts each result once
//! whichever shape wrote it.
//!
//! **Withheld results are not low retention.** A [`WireState::TriageFailed`] result
//! retained nothing because nothing was *recorded* (docs/triage.md), not
//! because triage was aggressive; folding it into the ratio would make a failed gate
//! read as excellent compression. It counts as [`Retention::withheld`] instead, and
//! stays out of both sides of the ratio.
//!
//! The fold is **monotone**: the log is append-only, and a result is counted once —
//! by its own entry when that carries a final wire, otherwise by the verdict that
//! names it — so a caller may consume entries incrementally from a cursor and never
//! needs to invalidate what it already folded.

use super::selection;
use crate::log::{LogEntry, Payload, RawResult, WireState};

/// Lines kept vs. lines triage was shown, plus the count of results whose output was
/// withheld outright. Cumulative over some span of the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Retention {
    /// Lines surviving on the wire, summed over triaged results.
    pub kept_lines: u64,
    /// Lines triage was shown, summed over the same results — the peek, so already
    /// capped at `PEEK_LINE_CAP`. Lines the cap dropped before triage ever saw them
    /// are neither shown nor kept, and are counted in [`Self::truncated_lines`].
    pub shown_lines: u64,
    /// Lines the peek cap dropped before triage saw them. Reported separately: they
    /// are not a decision anyone made, so they belong in neither side of the ratio.
    pub truncated_lines: u64,
    /// Results whose output was withheld because the gate failed
    /// ([`WireState::TriageFailed`]).
    pub withheld: u64,
}

impl Retention {
    /// Fold one log entry. Entries that aren't a result or a verdict about one, and
    /// results still projecting raw (untriaged — the tail, before its gate runs),
    /// contribute nothing. `log` is the whole log: a verdict is counted against the
    /// bytes at the result's own address, which is the one thing it does not carry.
    pub fn fold(&mut self, entry: &LogEntry, log: &[LogEntry]) {
        match &entry.payload {
            Payload::ToolResult { raw, wire, .. } => self.count(raw, wire),
            Payload::ResultVerdict { for_result, wire } => {
                // A verdict decides a captured result, and is ignored for one that
                // already carries a final wire — as projection ignores it
                // (docs/log-and-projection.md), so a result is counted once.
                if let Some(raw) = awaiting_verdict(log, *for_result) {
                    self.count(raw, wire);
                }
            }
            _ => {}
        }
    }

    /// Fold a whole span of entries.
    pub fn fold_all<'e>(
        &mut self,
        entries: impl IntoIterator<Item = &'e LogEntry>,
        log: &[LogEntry],
    ) {
        for entry in entries {
            self.fold(entry, log);
        }
    }

    /// Kept as a fraction of shown, or `None` before the first triaged result —
    /// which is *unknown*, not zero, and renders as absent rather than as 0%.
    pub fn ratio(&self) -> Option<f64> {
        (self.shown_lines > 0).then(|| self.kept_lines as f64 / self.shown_lines as f64)
    }

    /// Count one result under the wire it was decided with.
    fn count(&mut self, raw: &RawResult, wire: &WireState) {
        match wire {
            WireState::Triaged(verdict) => {
                let d = selection(&raw.content, &verdict.keep);
                self.kept_lines += d
                    .ranges
                    .iter()
                    .map(|&(s, e)| (e + 1 - s) as u64)
                    .sum::<u64>();
                self.shown_lines += d.shown.len() as u64;
                self.truncated_lines += d.dropped as u64;
            }
            WireState::TriageFailed { .. } => self.withheld += 1,
            // Awaiting triage (the captured result's own state until its verdict
            // lands), bypassed (the known-shape rule, docs/triage.md — never
            // shown to triage) and elided results are not verdicts about line-level
            // relevance, so none of them counts.
            WireState::Pending | WireState::Raw | WireState::Elided => {}
        }
    }
}

/// The bytes captured at `seq`, while that result is still waiting for its verdict —
/// where a `seq` is the result's own address in the log (docs/README.md).
fn awaiting_verdict(log: &[LogEntry], seq: u64) -> Option<&RawResult> {
    match log.get(seq as usize)?.payload {
        Payload::ToolResult {
            wire: WireState::Pending,
            ref raw,
            ..
        } => Some(raw),
        _ => None,
    }
}

#[cfg(test)]
mod test;
