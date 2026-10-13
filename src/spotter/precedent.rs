//! Which earlier calls ran with the operator's consent (docs/permissions.md).
//!
//! The decision rung clears a move only when enough earlier calls of the same
//! kind were approved by the operator. This module reads the log for those
//! calls; deciding whether one is the same action as the move is the model's job
//! (`jev/relation.rs`).

use crate::log::Telemetry;
use std::collections::{HashMap, HashSet};

use crate::log::{CallId, LogEntry, Payload, Stance, ToolCall};

/// The calls in `log` that ran with the operator's consent, in log order.
///
/// A call counts when it ran (a `ToolTelemetry` entry exists for it) and either
/// no spotter judged it (the fast path, a granted tool, or the human under
/// `human_only`) or the spotter held it with `Watch` or `Gate` and it ran anyway,
/// which is the operator approving. A call the spotter cleared (`Clear`,
/// `ClearAlways`) does not count: if it did, one clear would become the evidence
/// for the next.
pub fn consented(log: &[LogEntry]) -> Vec<ToolCall> {
    let mut ran: HashSet<&CallId> = HashSet::new();
    let mut judged: HashMap<&CallId, Stance> = HashMap::new();
    for entry in log {
        match &entry.payload {
            Payload::Telemetry(Telemetry::Tool(telemetry)) => {
                ran.insert(&telemetry.call_id);
            }
            Payload::Spot(spot) => {
                judged.insert(&spot.call_id, spot.stance);
            }
            _ => {}
        }
    }
    let consent = |id: &CallId| {
        ran.contains(id) && !matches!(judged.get(id), Some(Stance::Clear | Stance::ClearAlways))
    };
    log.iter()
        .filter_map(|entry| match &entry.payload {
            Payload::Assistant { output, .. } => output.tool_calls().ok(),
            _ => None,
        })
        .flatten()
        .filter(|call| consent(&call.id))
        .collect()
}

#[cfg(test)]
mod test;
