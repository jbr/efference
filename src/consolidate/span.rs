//! Working-span selection and accounting (docs/consolidate.md).
use super::NAME;
use crate::log::{Audience, CallId, LogEntry, Payload, Seq};
use crate::project::{Reader, UserPart, WireTurn, project_semantic, skipped_seqs};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: Seq,
    pub upto: Seq,
}

impl Span {
    pub fn contains(self, seq: Seq) -> bool {
        (self.from..=self.upto).contains(&seq)
    }
}

/// New work starts after the last adopted range, never before an earlier account.
pub fn start(log: &[LogEntry]) -> Option<Seq> {
    let skipped = skipped_seqs(log);
    let covered = log
        .iter()
        .filter(|e| !skipped.contains(&e.seq))
        .filter_map(|e| match &e.payload {
            Payload::Summary(summary) if Reader::Main.applies(summary.audience) => {
                Some(summary.upto)
            }
            _ => None,
        })
        .max();
    log.iter()
        .find(|e| {
            !skipped.contains(&e.seq)
                && covered.is_none_or(|c| e.seq > c)
                && matches!(
                    e.payload,
                    Payload::User(_) | Payload::Assistant { .. } | Payload::ToolResult { .. }
                )
        })
        .map(|e| e.seq)
}

/// Select work before the report/question or fresh user instruction at this juncture.
pub fn candidate(log: &[LogEntry]) -> Option<Span> {
    let skipped = skipped_seqs(log);
    let visible: Vec<_> = log.iter().filter(|e| !skipped.contains(&e.seq)).collect();
    let user = visible
        .iter()
        .rfind(|e| matches!(e.payload, Payload::User(_)))
        .map(|e| e.seq);
    let report = visible
        .iter()
        .rfind(|e| match &e.payload {
            Payload::Assistant {
                output,
                excluded: None,
            } => output.tool_calls().is_ok_and(|calls| {
                // A completion with neither prose nor a call (thinking only) ends the
                // trajectory without reporting anything, so it is not a juncture.
                calls.iter().any(|c| c.name == "ask")
                    || (calls.is_empty() && output.text().iter().any(|t| !t.trim().is_empty()))
            }),
            _ => false,
        })
        .map(|e| e.seq);
    // A reply after a final report retains both, whereas new work after an old report
    // can be considered up to a newly delivered instruction.
    let last_work = visible
        .iter()
        .rfind(|e| matches!(e.payload, Payload::ToolResult { .. }))
        .map(|e| e.seq);
    let anchor = match (user, report) {
        (Some(u), Some(r)) if u > r && last_work.is_some_and(|w| w > r && w < u) => u,
        (_, Some(r)) => r,
        (Some(u), None) => u,
        _ => return None,
    };
    let upto = visible.iter().rev().find(|e| e.seq < anchor)?.seq;
    before(log, upto)
}

pub fn before(log: &[LogEntry], upto: Seq) -> Option<Span> {
    let skipped = skipped_seqs(log);
    let upto = log
        .iter()
        .rev()
        .find(|e| {
            e.seq <= upto
                && !skipped.contains(&e.seq)
                && matches!(
                    e.payload,
                    Payload::User(_) | Payload::Assistant { .. } | Payload::ToolResult { .. }
                )
        })?
        .seq;
    let span = Span {
        from: start(log)?,
        upto,
    };
    (span.from <= span.upto && safe(log, span)).then_some(span)
}

/// Every visible call must have a projected result, and both must be on the same side.
pub fn safe(log: &[LogEntry], span: Span) -> bool {
    let turns = project_semantic(log);
    let results: HashMap<CallId, Seq> = turns
        .iter()
        .filter_map(|t| match t {
            WireTurn::User(parts) => Some(parts),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            UserPart::ToolResult {
                call_id,
                seq: Some(seq),
                ..
            } => Some((call_id.clone(), *seq)),
            _ => None,
        })
        .collect();
    let skipped = skipped_seqs(log);
    let elided: HashSet<_> = log
        .iter()
        .filter(|e| !skipped.contains(&e.seq))
        .filter_map(|e| match &e.payload {
            Payload::Summary(s) if s.audience == Audience::All => Some(s.range()),
            _ => None,
        })
        .flat_map(|range| {
            log.iter()
                .filter(move |e| range.contains(&e.seq))
                .map(|e| e.seq)
        })
        .collect();
    let visible: Vec<_> = log
        .iter()
        .filter(|e| !skipped.contains(&e.seq) && !elided.contains(&e.seq))
        .collect();
    let call_ids: HashSet<_> = visible
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Assistant {
                output,
                excluded: None,
            } => Some(output.tool_calls().unwrap_or_default()),
            _ => None,
        })
        .flatten()
        .map(|c| c.id)
        .collect();
    results.keys().all(|id| call_ids.contains(id))
        && visible.iter().all(|e| match &e.payload {
            Payload::Assistant {
                output,
                excluded: None,
            } => output.tool_calls().is_ok_and(|calls| {
                calls.iter().all(|c| {
                    results
                        .get(&c.id)
                        .is_some_and(|seq| span.contains(e.seq) == span.contains(*seq))
                })
            }),
            _ => true,
        })
}

/// Count UTF-8 arguments and post-verdict projected result text, not encoded images.
pub fn tool_bytes(log: &[LogEntry], span: Span) -> usize {
    let skipped = skipped_seqs(log);
    let calls: HashMap<CallId, (Seq, usize)> = log
        .iter()
        .filter(|e| !skipped.contains(&e.seq))
        .filter_map(|e| match &e.payload {
            Payload::Assistant {
                output,
                excluded: None,
            } => Some((e.seq, output.tool_calls().unwrap_or_default())),
            _ => None,
        })
        .flat_map(|(seq, calls)| {
            calls
                .into_iter()
                .filter(|c| c.name != NAME && !crate::triage::is_triage_tool(&c.name))
                .map(move |c| {
                    (
                        c.id,
                        (seq, serde_json::to_vec(&c.input).map_or(0, |v| v.len())),
                    )
                })
        })
        .collect();
    project_semantic(log)
        .iter()
        .filter_map(|t| match t {
            WireTurn::User(parts) => Some(parts),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            UserPart::ToolResult {
                call_id,
                seq: Some(seq),
                content,
                ..
            } if span.contains(*seq) => calls
                .get(call_id)
                .filter(|(call_seq, _)| span.contains(*call_seq))
                .map(|(_, bytes)| bytes + content.len()),
            _ => None,
        })
        .sum()
}

#[cfg(test)]
mod test;
