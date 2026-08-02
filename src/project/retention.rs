//! How much of what triage saw survives onto the wire (docs/redaction-fork.md).
//!
//! One rule, one place. A [`Payload::ToolResult`] carries both halves of the
//! question — `raw.content` (what the tool returned) and `wire` (the verdict on it)
//! — so retention is a fold over results alone, joining nothing. The per-result
//! split is [`distillate`]'s, so this reports the same line counts the transcript
//! renders in its gutter rather than a second, drifting definition of "kept".
//!
//! **Withheld results are not low retention.** A [`WireState::TriageFailed`] result
//! retained nothing because nothing was *recorded* (docs/redaction-fork.md), not
//! because triage was aggressive; folding it into the ratio would make a failed gate
//! read as excellent compression. It counts as [`Retention::withheld`] instead, and
//! stays out of both sides of the ratio.
//!
//! The fold is **monotone**: the log is append-only and a result's `wire` is fixed at
//! append time, so a caller may consume entries incrementally from a cursor and never
//! needs to invalidate what it already folded.

use super::distillate;
use crate::log::{LogEntry, Payload, WireState};

/// Lines kept vs. lines triage was shown, plus the count of results whose output was
/// withheld outright. Cumulative over some span of the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Retention {
    /// Lines surviving on the wire, summed over distilled results.
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
    /// Fold one log entry. Entries that aren't tool results, and results still
    /// projecting raw (untriaged — the tail, before its gate runs), contribute
    /// nothing.
    pub fn fold(&mut self, entry: &LogEntry) {
        let Payload::ToolResult { raw, wire, .. } = &entry.payload else {
            return;
        };
        match wire {
            WireState::Distilled { keep, .. } => {
                let d = distillate(&raw.content, keep);
                self.kept_lines += d.ranges.iter().map(|&(s, e)| (e + 1 - s) as u64).sum::<u64>();
                self.shown_lines += d.shown.len() as u64;
                self.truncated_lines += d.dropped as u64;
            }
            WireState::TriageFailed { .. } => self.withheld += 1,
            // Untriaged (the uncommitted tail) and elided results are not verdicts
            // about line-level relevance, so neither counts.
            WireState::Raw | WireState::Elided => {}
        }
    }

    /// Fold a whole span of entries.
    pub fn fold_all<'e>(&mut self, entries: impl IntoIterator<Item = &'e LogEntry>) {
        for entry in entries {
            self.fold(entry);
        }
    }

    /// Kept as a fraction of shown, or `None` before the first distilled result —
    /// which is *unknown*, not zero, and renders as absent rather than as 0%.
    pub fn ratio(&self) -> Option<f64> {
        (self.shown_lines > 0).then(|| self.kept_lines as f64 / self.shown_lines as f64)
    }
}

#[cfg(test)]
mod test;
