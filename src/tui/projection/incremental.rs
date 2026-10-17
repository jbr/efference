//! The transcript as a **materialized view** of the log, updated in place
//! (docs/tui.md).
//!
//! [`project`](super::project) rebuilds the whole document from the whole log and
//! stays the reference definition; this type keeps the document between repaints and
//! touches only what changed. It rests on one property of
//! [`entry_nodes`](super::entry_nodes): a committed entry's nodes are a pure function
//! of that entry, the annotations indexed against the calls it makes, and the human's
//! per-call view toggles — never of its neighbours. So the invalidation rule is a
//! single sentence:
//!
//! > **A committed entry is re-projected only when a call it owns changes,** or a
//! > summary whose range ends in it arrives.
//!
//! A payload that annotates a call in some new way joins the list of things that mark
//! an owner dirty. When in doubt [`invalidate_all`](Transcript::invalidate_all) is
//! always correct, just slower.

use std::collections::{BTreeSet, HashMap};

use super::{CallIndex, Gate, LiveTail, ViewState, entry_nodes, tail_nodes};
use crate::log::LogEntry;
use crate::tui::state::DocumentState;

/// The incremental transcript projection: what the document currently reflects, so
/// the next update can be a splice rather than a rebuild.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    /// How many top-level nodes each committed entry contributed, in log order.
    /// Its length is how much of the log has been projected.
    counts: Vec<usize>,
    /// `call_id` → the entry that *made* the call, i.e. whose nodes carry its
    /// call entry, its annotations, and its result.
    owner: HashMap<String, usize>,
    /// Result and spotter-verdict positions for the calls seen so far.
    index: CallIndex,
    /// Committed entries whose nodes are stale. Ascending, so a rebuild pass can
    /// keep running offsets straight as splices change later entries' positions.
    dirty: BTreeSet<usize>,
    /// The gate this document was last built with — a change dirties both the call
    /// it left and the call it moved to. The bound action is part of the value: a
    /// bound `agents__decide` moving onto the same call still changes the render.
    gate: Option<(String, Option<String>, Option<crate::tui::ActingOn>)>,
    /// The `ask` call the document was last built as current. The mark changes with
    /// no log entry, so a move dirties the ask it left and the one it reached.
    ask: Option<String>,
}

impl Transcript {
    /// Mark every committed entry stale — the next update rebuilds the document.
    /// The correct fallback whenever an invalidation is hard to pin down.
    pub(crate) fn invalidate_all(&mut self) {
        self.dirty.extend(0..self.counts.len());
    }

    /// Forget everything, so the next update projects the log from scratch. For a
    /// log that is *replaced* rather than appended to.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Bring `doc` up to date with the log, the live tail, and the human's view
    /// state, replacing only the nodes whose inputs changed.
    pub(in crate::tui) fn update<'a>(
        &mut self,
        doc: &mut DocumentState<'a>,
        log: &[LogEntry],
        live: &LiveTail,
        view: &mut ViewState,
        gate: Gate<'_>,
        pending_turns: &[crate::frontend::SubmittedTurn],
    ) {
        // A log that shrank or was replaced is not something this can splice.
        if log.len() < self.counts.len() {
            self.reset();
            doc.splice_nodes(0..doc.document.nodes.len(), Vec::new());
        }

        // Drop the previous tail up front, so the committed nodes are the whole
        // document while it is being updated and every offset is a plain prefix sum.
        let committed = self.counts.iter().sum::<usize>();
        doc.splice_nodes(committed..doc.document.nodes.len(), Vec::new());

        // The gate is ephemeral control state, not a log entry, so its movement has
        // to be noticed here: both the call it left and the call it arrived at
        // render differently (docs/tui.md).
        let held = self.gate.as_ref().map(|(call, reason, acting_on)| Gate {
            call: Some(call.as_str()),
            ask: gate.ask,
            reason: reason.as_deref(),
            acting_on: acting_on.as_ref(),
        });
        if held.unwrap_or_default() != gate {
            for call_id in [held.and_then(|g| g.call), gate.call].into_iter().flatten() {
                if let Some(&owner) = self.owner.get(call_id) {
                    self.dirty.insert(owner);
                }
            }
            self.gate = gate.call.map(|call| {
                (
                    call.to_string(),
                    gate.reason.map(str::to_string),
                    gate.acting_on.cloned(),
                )
            });
        }

        if self.ask.as_deref() != gate.ask {
            for call_id in [self.ask.as_deref(), gate.ask].into_iter().flatten() {
                if let Some(&owner) = self.owner.get(call_id) {
                    self.dirty.insert(owner);
                }
            }
            // The asks of one completion share an owner, but a topic clearing must
            // also refresh siblings that were `next`: they are dirtied through the
            // call they followed, which is the same entry.
            self.ask = gate.ask.map(str::to_string);
        }

        // Raw toggles, each keyed by the call whose result they change.
        for call_id in view.take_dirty() {
            if let Some(&owner) = self.owner.get(&call_id) {
                self.dirty.insert(owner);
            }
        }

        // Newly committed entries: index what they say about earlier calls (which
        // dirties those calls' owners), then project them onto the end.
        for idx in self.counts.len()..log.len() {
            if let Some(call_id) = self.index.absorb(idx, &log[idx])
                && let Some(&owner) = self.owner.get(&call_id)
            {
                self.dirty.insert(owner);
            }
            // A summary is drawn after the entry its range ends in, so its arrival
            // changes that earlier entry's nodes.
            if let Some(host) = self.index.summary_host(idx) {
                self.dirty.insert(host);
            }
            if let crate::log::Payload::Assistant { output, .. } = &log[idx].payload {
                for call in output.tool_calls().unwrap_or_default() {
                    self.owner.insert(call.id.0.clone(), idx);
                }
            }
            let nodes = entry_nodes(log, idx, &self.index, view, gate);
            self.counts.push(nodes.len());
            let at = doc.document.nodes.len();
            doc.splice_nodes(at..at, nodes);
        }

        // Rebuild the stale entries in place. Ascending order keeps the running
        // offset valid: entries before `idx` are already final, and `counts` is
        // updated as each splice lands.
        let dirty = std::mem::take(&mut self.dirty);
        let mut offset: usize = 0;
        let mut cursor = 0;
        for idx in dirty {
            if idx >= self.counts.len() {
                continue;
            }
            offset += self.counts[cursor..idx].iter().sum::<usize>();
            cursor = idx;
            let nodes = entry_nodes(log, idx, &self.index, view, gate);
            let was = self.counts[idx];
            self.counts[idx] = nodes.len();
            doc.splice_nodes(offset..offset + was, nodes);
        }

        // The tail is small and changes on nearly every event, so it is rebuilt
        // rather than tracked. It goes back on the end, where it was taken from.
        let at = doc.document.nodes.len();
        doc.splice_nodes(at..at, tail_nodes(live, view, pending_turns));
    }
}

#[cfg(test)]
mod test;
