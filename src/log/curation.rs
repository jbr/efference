//! Resolving captured results from their appended verdicts (docs/redaction-fork.md).

use std::collections::{HashMap, HashSet};

use super::{LogEntry, Payload, Seq, WireState};

/// Final verdicts for pending results. The first final verdict wins: a later
/// record cannot change a result already accepted into the cached prefix.
/// Verdicts never replace the state of results captured with a final state.
/// Lookup order cannot affect serialization (docs/log-and-projection.md).
pub fn result_verdicts(entries: &[LogEntry]) -> HashMap<Seq, &WireState> {
    let mut pending = HashSet::new();
    let mut verdicts = HashMap::new();
    for entry in entries {
        match &entry.payload {
            Payload::ToolResult {
                wire: WireState::Pending,
                ..
            } => {
                pending.insert(entry.seq);
            }
            Payload::ResultVerdict { for_result, wire }
                if pending.contains(for_result) && !matches!(wire, WireState::Pending) =>
            {
                verdicts.entry(*for_result).or_insert(wire);
            }
            _ => {}
        }
    }
    verdicts
}

#[cfg(test)]
mod test;
