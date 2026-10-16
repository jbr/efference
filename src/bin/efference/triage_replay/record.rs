//! Rebuilding recorded triages from a session log (docs/triage.md).

use efference::inbox::{ShownNotification, rider_after};
use efference::log::{Payload, ProcessEventKind, RawResult, ToolCall, WireState};
use std::collections::BTreeSet;

/// One recorded triage, rebuilt from the log: the prefix the triage saw, and the results it
/// was asked to triage.
///
/// The prefix ends **before** the triage's tool results, because that is where the live
/// loop snapshots it — `triage_messages` appends the results itself as fresh
/// `tool_result` blocks, so a prefix that already contained them would double them.
pub(crate) struct Recorded {
    /// Index into the log of the first result this triage carries — the prefix boundary.
    pub(crate) prefix_end: usize,
    /// Index into the log of the assistant turn whose calls this triage covers. A
    /// primer replays the main request's input, which ends *before* those calls —
    /// including them without their results is an unanswered `tool_use` the API
    /// rejects — so primer turns project `entries[..calls_at]`, not the triage prefix.
    pub(crate) calls_at: usize,
    /// The `seq` of the first result, which is how `--seq` addresses a triage and how the
    /// recorded `Triage.for_result` names it.
    pub(crate) seq: u64,
    pub(crate) calls: Vec<ToolCall>,
    pub(crate) raws: Vec<RawResult>,
    /// The recorded verdict per result, alongside `raws` — what the session's own
    /// triage chose to see. The reference a candidate model is compared against
    /// (backlog/inbox/distiller-experiment.md): not an operator's idea of the right keep,
    /// but what the agent that was doing the work kept for itself.
    pub(crate) recorded: Vec<WireState>,
    /// What the recorded run's first attempt did, from `Triage.retries` — the baseline
    /// each replay is compared against. `None` means it complied first try.
    pub(crate) recorded_first_failure: Option<String>,
    /// The notifications the live triage was shown (docs/inbox.md): those arrived by its
    /// `shown_through` mark and not resolved before it.
    pub(crate) inbox: Vec<ShownNotification>,
    /// Background processes spawned and not exited when the triage ran. Their output
    /// tails are sampled live and never logged, so a replay cannot rebuild that rider.
    pub(crate) running_processes: usize,
}

/// Rebuild every triage in a log, in order.
///
/// A triage is a completion's tool calls plus their results, which is exactly the grouping
/// the live loop opens a triage over: the triage's prefix ends with an assistant message
/// carrying one `tool_use` per call, and the API rejects a request that leaves any of
/// them unanswered (docs/triage.md).
pub(crate) fn recorded_triages(entries: &[efference::log::LogEntry]) -> Vec<Recorded> {
    // Every result, by call id, and every triage's recorded first-attempt breadcrumb. A
    // result logged `Pending` takes the verdict a later `ResultVerdict` gave it, as the
    // projection resolves it (docs/log-and-projection.md).
    let verdicts = efference::log::result_verdicts(entries);
    let mut triages = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let Payload::Assistant { output, .. } = &entry.payload else {
            continue;
        };
        let Ok(calls) = output.tool_calls() else {
            continue;
        };
        if calls.is_empty() {
            continue;
        }
        let ids: Vec<&str> = calls.iter().map(|c| c.id.0.as_str()).collect();
        let mut raws = Vec::new();
        let mut recorded = Vec::new();
        let mut kept_calls = Vec::new();
        let mut prefix_end = None;
        let mut seq = 0;
        for (j, later) in entries.iter().enumerate().skip(i + 1) {
            match &later.payload {
                Payload::ToolResult { call_id, raw, wire } if ids.contains(&call_id.0.as_str()) => {
                    if prefix_end.is_none() {
                        prefix_end = Some(j);
                        seq = later.seq;
                    }
                    if let Some(call) = calls.iter().find(|c| c.id == *call_id) {
                        kept_calls.push(call.clone());
                        raws.push(raw.clone());
                        recorded.push(
                            verdicts
                                .get(&later.seq)
                                .map_or_else(|| wire.clone(), |&verdict| verdict.clone()),
                        );
                    }
                }
                // The next completion starts a new triage.
                Payload::Assistant { .. } => break,
                _ => {}
            }
        }
        let Some(prefix_end) = prefix_end else {
            continue; // a completion whose calls never ran (aborted, withheld)
        };
        // The recorded first attempt, off this triage's `Triage` breadcrumbs. A batch
        // writes one `Triage` per result but its `retries` ride the **last** of them
        // (docs/triage.md), so every `Triage` between here and the next
        // completion is searched — reading only the first undercounts multi-result triages.
        let recorded_first_failure = entries[prefix_end..]
            .iter()
            .take_while(|e| !matches!(&e.payload, Payload::Assistant { .. }))
            .find_map(|e| match &e.payload {
                Payload::Triage(t) => t.retries.first().cloned(),
                _ => None,
            });
        // What the live triage passed as its inbox (docs/inbox.md): the notifications
        // arrived by its mark and not resolved before it, whether they were logged before
        // the prefix ended or between its results and the next completion.
        let after = entries[prefix_end..]
            .iter()
            .take_while(|e| !matches!(&e.payload, Payload::Assistant { .. }));
        let inbox = rider_after(entries, prefix_end);
        let mut running = BTreeSet::new();
        for e in entries[..prefix_end].iter().chain(after) {
            if let Payload::ProcessEvent(event) = &e.payload {
                match event.kind {
                    ProcessEventKind::Spawned(_) => running.insert(event.id),
                    ProcessEventKind::Exited(_) => running.remove(&event.id),
                };
            }
        }
        triages.push(Recorded {
            prefix_end,
            calls_at: i,
            seq,
            calls: kept_calls,
            raws,
            recorded,
            recorded_first_failure,
            inbox,
            running_processes: running.len(),
        });
    }
    triages
}

/// The session's own model, off the recorded preamble's identity line. The line
/// carries the session's id in a second sentence (docs/context.md), so the model
/// is the first sentence's body, not the whole line.
pub(crate) fn session_model(system: &str) -> Option<String> {
    let line = system.lines().next()?.strip_prefix("You are ")?;
    let model = line.split_once(". ").map_or(line, |(first, _)| first);
    model.strip_suffix('.').unwrap_or(model).to_string().into()
}
