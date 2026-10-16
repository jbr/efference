//! Compaction (backlog: cold-session.md, docs/summary.md): a cold session's earlier
//! history replaced by a summary the session's own model writes, with the most recent
//! payloads kept verbatim after it.
//!
//! `POST /loop/compact` asks; [`compact_loop`] does the work on the session's client,
//! beside the keepalive. A compaction appends one [`Summary`] made by `Compaction` and
//! never starts a request of its own afterwards: an idle session stays idle, and a loop
//! parked at its cold check stays parked until the operator's next turn or continue.

use super::operations::CompactionState;
use super::*;
use crate::log::{Audience, LogEntry, Seq, Summary, Telemetry, Timestamp};

/// Roughly how many tokens of the most recent history a compaction keeps verbatim. Estimated
/// from each entry's recorded size, which overstates a result its verdict trimmed, so
/// the tail errs short.
pub(crate) const COMPACTION_TAIL_TOKENS: usize = 15_000;

/// Why a `POST /loop/compact` was declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotCompactable {
    /// The cache is not believed expired: a compaction is the cold session's alternative
    /// to an uncached read, and on a warm one it would throw the cache away.
    NotCold,
    Gated,
    Asking,
    /// A request is in flight.
    Busy,
    /// Everything after the last compaction fits in the verbatim tail.
    Nothing,
}

impl NotCompactable {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NotCold => {
                "nothing to compact: the cache is not believed expired (compacting a warm session is asked for with warm: true)"
            }
            Self::Gated => "an approval gate is pending — resolve it first",
            Self::Asking => "a question is pending — answer it first",
            Self::Busy => "the loop is running — compact when it stops",
            Self::Nothing => {
                "nothing to compact: the conversation is already shorter than what a compaction keeps verbatim"
            }
        }
    }
}

/// Where a compaction should cut `entries`: the last seq the summary covers, leaving roughly
/// `budget` tokens of recent payloads after it verbatim. Moved earlier until no call is
/// separated from its result (a summary there could not project, docs/consolidate.md).
/// `None` when nothing after the latest compaction would be left to summarize.
pub(crate) fn compaction_cut(entries: &[LogEntry], budget: usize) -> Option<Seq> {
    let mut tokens = 0;
    let mut cut = None;
    for (i, entry) in entries.iter().enumerate().rev() {
        tokens += serde_json::to_string(&entry.payload).map_or(0, |s| s.len() / 4);
        if tokens >= budget {
            cut = i.checked_sub(1).map(|j| entries[j].seq);
            break;
        }
    }
    let mut cut = cut?;
    while !crate::consolidate::split_calls(entries, cut).is_empty() {
        cut = entries.iter().rev().map(|e| e.seq).find(|&seq| seq < cut)?;
    }
    // Something besides earlier summaries has to lie between the last compaction and
    // the cut, or there is nothing new for a summary to cover.
    let covered = last_compaction_upto(entries);
    entries
        .iter()
        .any(|e| {
            covered.is_none_or(|c| e.seq > c)
                && e.seq <= cut
                && !matches!(e.payload, Payload::Summary(_))
        })
        .then_some(cut)
}

/// The `upto` of the latest head summary (one that starts at the first entry) on the
/// main projection.
fn last_compaction_upto(entries: &[LogEntry]) -> Option<Seq> {
    entries.iter().rev().find_map(|e| match &e.payload {
        Payload::Summary(summary) if summary.from == 0 && summary.audience == Audience::All => {
            Some(summary.upto)
        }
        _ => None,
    })
}

impl Session {
    /// When the main projection's prefix last changed under a summary: the latest
    /// summary entry's timestamp. A request older than this read a prefix that no
    /// longer exists, so it says nothing about the cache (`cold.rs`).
    pub(crate) fn last_compaction_at(&self) -> Option<Timestamp> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find(|e| {
                matches!(
                    &e.payload,
                    Payload::Summary(Summary {
                        audience: Audience::All,
                        ..
                    })
                )
            })
            .map(|e| e.at)
    }

    /// The compaction open now, or the last one's failure, folded from the log
    /// (`operations.rs`).
    pub(crate) fn compaction_state(&self) -> Option<CompactionState> {
        self.operations.lock().unwrap().compaction().cloned()
    }

    /// The `compaction_failure` topic: `{error, at}` after a compaction failed, until a
    /// turn's request goes out or another compaction is asked for (docs/summary.md).
    pub(super) fn compaction_failure_json(&self) -> Value {
        match self.compaction_state() {
            Some(CompactionState::Failed { error, at }) => {
                serde_json::json!({"error": error, "at": at})
            }
            _ => Value::Null,
        }
    }

    pub(crate) fn compacting(&self) -> bool {
        self.compaction_state().is_some_and(|state| state.open())
    }

    /// The cut of the compaction waiting to be written, if one is.
    fn waiting_compaction(&self) -> Option<Seq> {
        match self.compaction_state()? {
            CompactionState::Waiting { cut, .. } => Some(cut),
            _ => None,
        }
    }

    /// Whether `POST /loop/compact` accepts now with this `warm`, and at which cut — the
    /// route's whole precondition, which the `verbs` topic publishes
    /// (docs/control-api.md). `Ok(None)` is a compaction already open, accepted again as
    /// the same request. A loop parked at its cold check or held at a triage sends
    /// nothing while the summary is written, so either accepts a compaction; the held
    /// triage's retry then triages the compacted projection. `warm` is the operator's
    /// explicit choice to compact a session whose cache is not believed expired, giving
    /// up the cached prefix; without it a warm session declines.
    pub(crate) fn compaction_verdict(&self, warm: bool) -> Result<Option<Seq>, NotCompactable> {
        if self.compacting() {
            return Ok(None);
        }
        match self.loop_phase() {
            LoopPhase::Gated => return Err(NotCompactable::Gated),
            LoopPhase::Asking => return Err(NotCompactable::Asking),
            LoopPhase::Idle
            | LoopPhase::ColdParked
            | LoopPhase::Blocked {
                reason: BlockedReason::TriageUnavailable | BlockedReason::TriageDefect,
            } => {}
            _ => return Err(NotCompactable::Busy),
        }
        if !warm && self.cold_since().is_none() {
            return Err(NotCompactable::NotCold);
        }
        let log = self.log.lock().unwrap();
        match compaction_cut(log.entries(), COMPACTION_TAIL_TOKENS) {
            Some(cut) => Ok(Some(cut)),
            None => Err(NotCompactable::Nothing),
        }
    }

    /// Ask for a compaction (`POST /loop/compact`), or say why it cannot run
    /// ([`Self::compaction_verdict`]).
    pub(crate) fn request_compaction(&self, warm: bool) -> Result<(), NotCompactable> {
        let Some(cut) = self.compaction_verdict(warm)? else {
            return Ok(());
        };
        self.record_compaction(Telemetry::CompactionRequested { warm, cut });
        self.compaction_signal.notify_one();
        Ok(())
    }

    /// Wait until no compaction is open: the compact route's `?wait`, which answers once
    /// the summary lands or the compaction fails (docs/control-api.md).
    pub(super) async fn await_compaction(&self) {
        loop {
            let notified = self.settle_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.compacting() {
                return;
            }
            notified.await;
        }
    }

    /// Append one compaction event and wake whoever reads the state it folds into
    /// (docs/summary.md). A failed write is reported and dropped, like the cold
    /// check's records.
    fn record_compaction(&self, event: Telemetry) {
        let turn = self.latest_turn();
        if let Err(e) = self.append(turn, Payload::Telemetry(event)) {
            log::warn!("compaction record: append failed: {e}");
        }
        self.settle_changed.notify_waiters();
        self.touch_state();
    }
}

/// Write compactions as they are asked for, on the session's own client (docs/summary.md).
/// One at a time, at the cut its `CompactionRequested` recorded; the outcome is the
/// compaction's `Summary` or a `CompactionFailed`.
pub(crate) async fn compact_loop(client: Arc<Client>, session: Arc<Session>, tools: Vec<Value>) {
    loop {
        session.compaction_signal.notified().await;
        let Some(cut) = session.waiting_compaction() else {
            continue;
        };
        match compact_once(&client, &session, &tools, cut).await {
            Ok(()) => {
                session.settle_changed.notify_waiters();
                session.touch_state();
            }
            Err(error) => session.record_compaction(Telemetry::CompactionFailed { error }),
        }
    }
}

async fn compact_once(
    client: &Client,
    session: &Session,
    tools: &[Value],
    cut: Seq,
) -> Result<(), String> {
    let entries = session.snapshot();
    let turns_before = session.metrics.lock().unwrap().turn_requests;
    // The summary as it is written goes to `output` subscribers and nowhere else: the
    // entry below stays its only durable record (docs/summary.md).
    let compaction =
        crate::compact::summarize(
            client,
            tools,
            &entries,
            Some(cut),
            |streamed| match streamed {
                Streamed::Text { text, .. } => {
                    session.publish_delta(StreamDelta::CompactionText { text })
                }
                Streamed::Thinking { text, .. } => {
                    session.publish_delta(StreamDelta::CompactionThinking { text })
                }
                Streamed::ToolStart { .. } | Streamed::ToolInput { .. } => {}
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    session.record_request(RequestOrigin::Compaction, compaction.usage);
    // A turn that reached the cache while the summary was being written (the operator
    // paid the full read meanwhile) makes the compaction moot. A keepalive ping does
    // not: on a warm compaction it is the cache being kept, not read for a turn.
    if session.metrics.lock().unwrap().turn_requests != turns_before {
        return Err("superseded: a request read the cache while the summary was written".into());
    }
    let now = session.snapshot();
    if !crate::consolidate::split_calls(&now, cut).is_empty() {
        return Err("superseded: the log moved across the cut".into());
    }
    let model = crate::log::recorded_inference(&now)
        .cloned()
        .ok_or("the log records no boot identity to name the model")?;
    let summary = Summary::compaction(cut, compaction.text, model);
    session
        .append(session.latest_turn(), Payload::Summary(summary))
        .map_err(|e| format!("appending the compaction: {e}"))?;
    Ok(())
}
