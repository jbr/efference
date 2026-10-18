//! What the conversation and the zoom look up across the whole log
//! (docs/control-api.md): each call by its id, with its handle, and what became of it —
//! its result, the verdict that decided a captured result, the gate's recorded
//! decision, the spotter's verdict — wherever in the log those landed. A window may be
//! cut anywhere, so these lookups never stop at its edges.

use super::*;
use crate::frontend::Approval;
use crate::log::{ContentBlock, GateDecision, Seq, Spot, Telemetry};

/// One call, as the log issued it.
pub(super) struct Call {
    /// The assistant entry that issued it.
    pub(super) seq: Seq,
    /// `seq`, or `seq.n` when the entry issued several ([`call_label`]).
    pub(super) handle: String,
    pub(super) call: ToolCall,
    /// The question and its place among the entry's asks, when the call is an `ask`
    /// (`process_monitor::ask_of_call`).
    pub(super) ask: Option<(String, usize, usize)>,
}

/// The lookups, built in one pass over the log.
#[derive(Default)]
pub(super) struct Index<'a> {
    log: &'a [LogEntry],
    calls: HashMap<String, Call>,
    results: HashMap<String, &'a LogEntry>,
    verdicts: HashMap<Seq, &'a WireState>,
    decisions: HashMap<String, (Seq, &'a GateDecision)>,
    spots: HashMap<String, &'a Spot>,
    /// The loop-appended `User` entry that carries a redirect's message, by its seq,
    /// to the redirected call's id. The decision names who wrote the message; the
    /// entry itself carries no source.
    steers: HashMap<Seq, String>,
}

impl<'a> Index<'a> {
    pub(super) fn new(log: &'a [LogEntry]) -> Self {
        let mut index = Self {
            log,
            ..Self::default()
        };
        // Redirects whose message has not yet landed as its user turn, in order.
        let mut redirects: Vec<(&'a str, String)> = Vec::new();
        for entry in log {
            match &entry.payload {
                // A record-only turn was never dispatched; its calls have no outcome.
                Payload::Assistant {
                    output,
                    excluded: None,
                } => {
                    let (_, calls) = assistant(output);
                    for (i, call) in calls.iter().enumerate() {
                        let ask = crate::process_monitor::ask_of_call(&calls, i);
                        let indexed = Call {
                            seq: entry.seq,
                            handle: call_label(entry.seq, i, calls.len()),
                            call: call.clone(),
                            ask,
                        };
                        index.calls.insert(call.id.0.clone(), indexed);
                    }
                }
                Payload::ToolResult { call_id, .. } => {
                    index.results.insert(call_id.0.clone(), entry);
                }
                Payload::ResultVerdict { for_result, wire } => {
                    index.verdicts.insert(*for_result, wire);
                }
                Payload::Spot(spot) => {
                    index.spots.insert(spot.call_id.0.clone(), spot);
                }
                Payload::Telemetry(Telemetry::GateDecision(decision)) => {
                    if let Approval::Redirect(message) = &decision.decision {
                        redirects.push((message, decision.call_id.0.clone()));
                    }
                    index
                        .decisions
                        .insert(decision.call_id.0.clone(), (entry.seq, decision));
                }
                // The loop appends a redirect's message as a user turn of its own, with
                // no submission and no source (`agent/landing.rs`).
                Payload::User(blocks)
                    if entry.submission_id.is_none() && entry.source.is_none() =>
                {
                    if let [ContentBlock::Text(text)] = blocks.as_slice()
                        && let Some(at) = redirects.iter().position(|(message, _)| message == text)
                    {
                        let (_, call_id) = redirects.remove(at);
                        index.steers.insert(entry.seq, call_id);
                    }
                }
                _ => {}
            }
        }
        index
    }

    pub(super) fn call(&self, id: &str) -> Option<&Call> {
        self.calls.get(id)
    }

    /// The call's result entry.
    pub(super) fn result(&self, id: &str) -> Option<&'a LogEntry> {
        self.results.get(id).copied()
    }

    /// A result entry's final wire: its own, or for a captured result the verdict that
    /// decided it; `None` while nothing has (docs/triage.md).
    pub(super) fn wire(&self, result: &'a LogEntry) -> Option<&'a WireState> {
        let Payload::ToolResult { wire, .. } = &result.payload else {
            return None;
        };
        match wire {
            WireState::Pending => self.verdicts.get(&result.seq).copied(),
            wire => Some(wire),
        }
    }

    /// The gate's recorded decision on the call, with the entry that records it.
    pub(super) fn decision(&self, id: &str) -> Option<(Seq, &'a GateDecision)> {
        self.decisions.get(id).copied()
    }

    /// Whether the call's recorded decision refused it.
    pub(super) fn refused(&self, id: &str) -> bool {
        self.decision(id).is_some_and(|(_, d)| refused(d))
    }

    /// The spotter's latest verdict on the call.
    pub(super) fn spot(&self, id: &str) -> Option<&'a Spot> {
        self.spots.get(id).copied()
    }

    /// The call whose redirect message the user entry at `seq` carries.
    pub(super) fn steer(&self, seq: Seq) -> Option<&str> {
        self.steers.get(&seq).map(String::as_str)
    }

    /// The entry at `seq`.
    pub(super) fn entry(&self, seq: Seq) -> Option<&'a LogEntry> {
        let at = self.log.partition_point(|entry| entry.seq < seq);
        self.log.get(at).filter(|entry| entry.seq == seq)
    }

    /// `ok, 12 lines, it saw 1: note` — the result's outcome, size, and what the
    /// session saw of it; the last part is left out when there was nothing to see.
    pub(super) fn result_words(&self, result: &'a LogEntry) -> String {
        let Payload::ToolResult { raw, .. } = &result.payload else {
            return String::new();
        };
        let outcome = outcome(raw);
        if raw.content.is_empty() && raw.images.is_empty() {
            return outcome;
        }
        format!("{outcome}, {}", saw(raw, self.wire(result), false))
    }
}
