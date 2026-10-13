//! What a log's last completion left undone (docs/resumption.md).
//!
//! One reading of the log, shared by the loader (`boot::resume`, which decides where a
//! reopened session continues from) and the agent loop (which decides whether the
//! operator's first action replays a pending batch). Both derive the answer from the
//! entries each time rather than from a flag that could drift.
//!
//! Each call of the last live completion is in one of four states ([`CallState`]). A call
//! that **started and has no result** may have touched the world, so the session rolls
//! back past the whole completion. Any other unfinished completion is a **pending batch**:
//! its undecided calls have not run, and the session resumes at it.

use crate::log::Telemetry;
use crate::log::{CallId, LogEntry, Payload, Seq, Stance, ToolCall, Turn, WireState};
use crate::project::skipped_seqs;

/// Where one call of a completion stands in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    /// A result with its decision: nothing left to do.
    Decided,
    /// A `Pending` result with no `ResultVerdict`: triaged at the resumed gate, the call
    /// does not run again.
    Captured { result_seq: Seq },
    /// No result and no sign it ran: goes through the permission cascade again.
    NeverStarted,
    /// `CallStarted` and no result: the crash case, whose outcome is unknown.
    Started,
}

/// The last live completion that carried calls, with the state of each call in call order.
#[derive(Clone, Debug)]
pub struct Completion {
    pub seq: Seq,
    pub turn: Turn,
    pub calls: Vec<(ToolCall, CallState)>,
}

impl Completion {
    /// Whether any call started without a result: the case that rolls back.
    pub fn has_started(&self) -> bool {
        self.calls.iter().any(|(_, s)| *s == CallState::Started)
    }

    /// Whether every call is decided (vacuously true for no calls).
    pub fn is_clean(&self) -> bool {
        self.calls.iter().all(|(_, s)| *s == CallState::Decided)
    }

    /// The calls a resume at this batch brings back: captured and never started.
    pub fn pending_ids(&self) -> Vec<CallId> {
        self.calls
            .iter()
            .filter(|(_, s)| matches!(s, CallState::Captured { .. } | CallState::NeverStarted))
            .map(|(call, _)| call.id.clone())
            .collect()
    }
}

/// Where a reopened session continues from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resumption {
    /// `Some(seq)` of the last entry before a rolled-back completion; `None` when nothing
    /// is skipped.
    pub resumes_after: Option<Seq>,
    /// The calls of the last live completion that are still to come back, in call order;
    /// empty when the log ends cleanly or rolled back past its batch.
    pub pending: Vec<CallId>,
}

/// Whether the log has any `CallStarted` marker at all. A log from before the marker
/// existed has none, which changes how a resultless call is read.
fn has_markers(entries: &[LogEntry]) -> bool {
    entries
        .iter()
        .any(|e| matches!(e.payload, Payload::Telemetry(Telemetry::CallStarted { .. })))
}

/// The index in `live` of the last completion that is live and not record-only.
fn last_completion(live: &[LogEntry], is_live: &impl Fn(&LogEntry) -> bool) -> Option<usize> {
    live.iter().rposition(|e| {
        is_live(e)
            && matches!(
                &e.payload,
                Payload::Assistant {
                    upstream_failure: None,
                    ..
                }
            )
    })
}

/// Classify the calls of the assistant entry at `live[index]` against the live entries
/// after it. With `markers` (the log has `CallStarted` entries), a resultless call is
/// started iff it has one. Without, a resultless call is never started iff a `Spot` for it
/// stances `gate` (or `watch`, which resolves to the same human gate), and started otherwise.
fn classify(
    live: &[LogEntry],
    index: usize,
    is_live: &impl Fn(&LogEntry) -> bool,
    markers: bool,
) -> Result<Option<Completion>, String> {
    let completion = &live[index];
    let Payload::Assistant { output, .. } = &completion.payload else {
        return Ok(None);
    };
    let calls = output
        .tool_calls()
        .map_err(|e| format!("the last completion's calls do not parse: {e}"))?;
    let after = || live[index + 1..].iter().filter(|e| is_live(e));
    let classified = calls
        .into_iter()
        .map(|call| {
            let result = after().find_map(|entry| match &entry.payload {
                Payload::ToolResult { call_id, wire, .. } if *call_id == call.id => {
                    Some((entry.seq, wire))
                }
                _ => None,
            });
            let state = match result {
                Some((seq, wire)) => {
                    let decided = !matches!(wire, WireState::Pending)
                        || after().any(|entry| {
                            matches!(&entry.payload, Payload::ResultVerdict { for_result, .. } if *for_result == seq)
                        });
                    if decided {
                        CallState::Decided
                    } else {
                        CallState::Captured { result_seq: seq }
                    }
                }
                None => {
                    let never_started = if markers {
                        !after().any(|entry| {
                            matches!(&entry.payload, Payload::Telemetry(Telemetry::CallStarted { call_id }) if *call_id == call.id)
                        })
                    } else {
                        after().any(|entry| {
                            matches!(&entry.payload, Payload::Spot(spot)
                                if spot.call_id == call.id
                                    && matches!(spot.stance, Stance::Gate | Stance::Watch))
                        })
                    };
                    if never_started {
                        CallState::NeverStarted
                    } else {
                        CallState::Started
                    }
                }
            };
            (call, state)
        })
        .collect();
    Ok(Some(Completion {
        seq: completion.seq,
        turn: completion.turn,
        calls: classified,
    }))
}

/// Where the session continues from: after the last entry when the log ends cleanly or
/// at a pending batch, else from just before the completion holding a started call, and
/// the same check runs on the log up to there until it ends in one of the first two
/// (docs/resumption.md). Entries an earlier `Resumed` skipped are not live: they are
/// neither the completion nor the rollback point. The error is the reason a log has
/// nothing to continue from.
pub fn resume_point(entries: &[LogEntry]) -> Result<Resumption, String> {
    let markers = has_markers(entries);
    let mut end = entries.len();
    let mut rolled_back = None;
    loop {
        let live = &entries[..end];
        let skipped = skipped_seqs(live);
        let is_live = |e: &LogEntry| !skipped.contains(&e.seq);
        let Some(index) = last_completion(live, &is_live) else {
            return Ok(Resumption {
                resumes_after: rolled_back,
                pending: Vec::new(),
            });
        };
        let completion = classify(live, index, &is_live, markers)?
            .expect("an assistant entry classifies as a completion");
        if !completion.has_started() {
            return Ok(Resumption {
                resumes_after: rolled_back,
                pending: completion.pending_ids(),
            });
        }
        let Some(before) = live[..index].iter().rposition(is_live) else {
            return Err("the log ends mid-batch with nothing before it to resume from".into());
        };
        rolled_back = Some(live[before].seq);
        end = before + 1;
    }
}

/// The last live completion with each call's state, or `None` for a log with no
/// completion in it.
pub fn last_live_completion(entries: &[LogEntry]) -> Result<Option<Completion>, String> {
    let skipped = skipped_seqs(entries);
    let is_live = |e: &LogEntry| !skipped.contains(&e.seq);
    let Some(index) = last_completion(entries, &is_live) else {
        return Ok(None);
    };
    classify(entries, index, &is_live, has_markers(entries))
}

/// The last live completion when it is a pending batch: it has calls still to come back
/// and none that started without a result. What the agent loop replays on the operator's
/// first action, and what keeps a continue from declining on an unanswered `tool_use`.
/// `None` for a log that ends cleanly, or for a started call (which a resume rolls back
/// and a live session never leaves behind).
pub fn pending_batch(entries: &[LogEntry]) -> Option<Completion> {
    let completion = last_live_completion(entries).ok().flatten()?;
    (!completion.has_started() && !completion.is_clean()).then_some(completion)
}
