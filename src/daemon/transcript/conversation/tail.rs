//! A tail of the conversation grain counts rendered lines (docs/control-api.md): the
//! last `n` lines of a range, taken whole entry by entry and extended back until no
//! result, verdict, decision, note or redirect message in the window is without the
//! call it belongs to. A cut by log entries would end on mechanism or open on an
//! outcome whose call is out of view.

use super::*;

/// Where a tail starts.
pub(super) struct Cut {
    /// The index in the range of the first entry kept.
    pub(super) start: usize,
    /// How many rendered lines come before it in the range.
    pub(super) earlier: usize,
}

/// The cut a tail of `n` lines makes of `range`, or `None` when the range holds no more
/// than `n` lines (and when `n` is 0, which keeps nothing and is the caller's to read).
pub(super) fn cut(log: &[LogEntry], range: &[LogEntry], n: u64, reading: &Reading) -> Option<Cut> {
    if n == 0 || range.is_empty() {
        return None;
    }
    let out = Out::run(log, range, reading);
    let first_kept = out.lines.len().checked_sub(usize::try_from(n).ok()?)?;
    if first_kept == 0 {
        return None;
    }
    let start = out.closed_start(range, out.lines[first_kept].0);
    let kept_from = range[start].seq;
    let earlier = out.lines.iter().filter(|(seq, _)| *seq < kept_from).count();
    (start > 0).then_some(Cut { start, earlier })
}

impl Out<'_, '_> {
    /// The index in `range` of the entry at `origin`, moved back until every entry from
    /// there on has its call in the window too.
    fn closed_start(&self, range: &[LogEntry], origin: Seq) -> usize {
        let mut start = range.partition_point(|entry| entry.seq < origin);
        // Entries from `scanned` on have been checked against the start they were
        // checked for; a move back checks only what it newly covers.
        let mut scanned = range.len();
        loop {
            let earliest = range[start..scanned]
                .iter()
                .filter_map(|entry| self.call_seq(entry))
                .min();
            scanned = start;
            let Some(seq) = earliest else { break };
            let back = range.partition_point(|entry| entry.seq < seq);
            if back >= start {
                break;
            }
            start = back;
        }
        start
    }

    /// The seq of the call whose outcome `entry` is, when it is one and the call was
    /// issued: a result, a verdict on a result, a gate decision, the user turn that
    /// carries a redirect's message or an allow's note.
    fn call_seq(&self, entry: &LogEntry) -> Option<Seq> {
        let of = |call_id: &str| self.index.call(call_id).map(|call| call.seq);
        match &entry.payload {
            Payload::ToolResult { call_id, .. } => of(&call_id.0),
            Payload::ResultVerdict { for_result, .. } => {
                match &self.index.entry(*for_result)?.payload {
                    Payload::ToolResult { call_id, .. } => of(&call_id.0),
                    _ => None,
                }
            }
            Payload::Telemetry(Telemetry::GateDecision(decision)) => of(&decision.call_id.0),
            Payload::User(_) => self
                .index
                .steer(entry.seq)
                .or_else(|| self.index.note_for(entry.seq))
                .and_then(of),
            _ => None,
        }
    }
}

#[cfg(test)]
mod test;
