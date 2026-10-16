//! The fold (backlog: cold-session.md, docs/summary-head.md): a cold session's earlier
//! history replaced by an account the session's own model writes, with the most recent
//! payloads kept verbatim after it.
//!
//! `POST /loop/fold` asks; [`fold_loop`] does the work on the session's client, beside
//! the keepalive. The fold appends one `Mark::Boundary` written by the narrator and
//! never starts a request of its own afterwards: an idle session stays idle, and a loop
//! parked at its cold check stays parked until the operator's next turn or continue.

use super::*;
use crate::log::{Audience, LogEntry, Mark, MarkKind, Seq, Timestamp, Writer};

/// Roughly how many tokens of the most recent history a fold keeps verbatim. Estimated
/// from each entry's recorded size, which overstates a result its verdict trimmed, so
/// the tail errs short.
pub(crate) const FOLD_TAIL_TOKENS: usize = 15_000;

/// The fold in progress, or the last one's failure, as the `cold` topic's `fold` field.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum FoldState {
    Writing,
    Failed { error: String },
}

/// Why a `POST /loop/fold` was declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotFoldable {
    /// The cache is not believed expired: a fold is the cold session's alternative to
    /// an uncached read, and on a warm one it would throw the cache away.
    NotCold,
    Gated,
    Asking,
    /// A request is in flight.
    Busy,
    /// Everything after the last fold fits in the verbatim tail.
    Nothing,
}

impl NotFoldable {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NotCold => {
                "nothing to fold: the cache is not believed expired (a fold of a warm session is asked for with warm: true)"
            }
            Self::Gated => "an approval gate is pending — resolve it first",
            Self::Asking => "a question is pending — answer it first",
            Self::Busy => "the loop is running — fold when it stops",
            Self::Nothing => {
                "nothing to fold: the conversation is already shorter than what a fold keeps verbatim"
            }
        }
    }
}

/// Where a fold should cut `entries`: the last seq the account covers, leaving roughly
/// `budget` tokens of recent payloads after it verbatim. Moved earlier until no call is
/// separated from its result (a boundary there could not project, docs/consolidate.md).
/// `None` when nothing after the latest fold would be left to summarize.
pub(crate) fn fold_cut(entries: &[LogEntry], budget: usize) -> Option<Seq> {
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
    // Something besides earlier folds' own marks has to lie between the last fold and
    // the cut, or there is nothing new for an account to cover.
    let covered = last_fold_upto(entries);
    entries
        .iter()
        .any(|e| {
            covered.is_none_or(|c| e.seq > c)
                && e.seq <= cut
                && !matches!(e.payload, Payload::Mark(_))
        })
        .then_some(cut)
}

/// The `upto` of the latest boundary on the main projection.
fn last_fold_upto(entries: &[LogEntry]) -> Option<Seq> {
    entries.iter().rev().find_map(|e| match &e.payload {
        Payload::Mark(Mark {
            kind: MarkKind::Boundary { upto },
            audience: Audience::All,
            ..
        }) => Some(*upto),
        _ => None,
    })
}

impl Session {
    /// When the main projection's prefix last changed under a fold: the latest
    /// boundary mark's timestamp. A request older than this read a prefix that no
    /// longer exists, so it says nothing about the cache (`cold.rs`).
    pub(crate) fn last_fold_at(&self) -> Option<Timestamp> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find(|e| {
                matches!(
                    &e.payload,
                    Payload::Mark(Mark {
                        audience: Audience::All,
                        ..
                    })
                )
            })
            .map(|e| e.at)
    }

    pub(crate) fn fold_state(&self) -> Option<FoldState> {
        self.fold_state.lock().unwrap().clone()
    }

    /// The `fold` topic: `{state: "writing"}` while an account is being written,
    /// `{state: "failed", error}` after one failed until the next turn or fold, else
    /// `null`. Independent of the `cold` topic: a fold can run on a warm session.
    pub(super) fn fold_json(&self) -> Value {
        serde_json::to_value(self.fold_state()).unwrap_or(Value::Null)
    }

    /// Drop a failed fold's report once the session moves on: a turn starting makes
    /// the failure stale. A fold still writing is left alone.
    pub(super) fn clear_failed_fold(&self) {
        let mut state = self.fold_state.lock().unwrap();
        if matches!(*state, Some(FoldState::Failed { .. })) {
            *state = None;
            drop(state);
            self.touch_state();
        }
    }

    pub(crate) fn folding(&self) -> bool {
        self.fold_state() == Some(FoldState::Writing)
    }

    /// Whether the loop is held at a triage gate that produced no verdict
    /// (docs/triage.md): running, but sending nothing until the operator
    /// retries. The held gate is the one stall set while the loop runs.
    pub(crate) fn held_at_gate(&self) -> bool {
        self.stall().is_some_and(|stall| {
            matches!(
                stall.reason,
                StallReason::TriageUnavailable | StallReason::TriageDefect
            )
        })
    }

    /// Ask for a fold (`POST /loop/fold`), or say why it cannot run. A fold already
    /// writing is accepted again, as one request. A loop parked at its cold check or
    /// held at a triage gate sends nothing while the account is written, so either
    /// accepts a fold; the held gate's retry then peeks the folded projection. `warm` is the operator's explicit
    /// choice to fold a session whose cache is not believed expired, giving up the
    /// cached prefix; without it a warm session declines.
    pub(crate) fn request_fold(&self, warm: bool) -> Result<(), NotFoldable> {
        if self.folding() {
            return Ok(());
        }
        if self.pending.lock().unwrap().is_some() {
            return Err(NotFoldable::Gated);
        }
        if self.asking().is_some() {
            return Err(NotFoldable::Asking);
        }
        if self.running.load(Ordering::SeqCst) && !self.cold_parked() && !self.held_at_gate() {
            return Err(NotFoldable::Busy);
        }
        if !warm && self.cold_since().is_none() {
            return Err(NotFoldable::NotCold);
        }
        if fold_cut(&self.snapshot(), FOLD_TAIL_TOKENS).is_none() {
            return Err(NotFoldable::Nothing);
        }
        *self.fold_state.lock().unwrap() = Some(FoldState::Writing);
        self.fold_signal.notify_one();
        self.settle_changed.notify_waiters();
        self.touch_state();
        Ok(())
    }

    fn finish_fold(&self, outcome: Option<FoldState>) {
        *self.fold_state.lock().unwrap() = outcome;
        self.settle_changed.notify_waiters();
        self.touch_state();
    }
}

/// Write folds as they are asked for, on the session's own client (docs/summary-head.md).
/// One at a time; the request's outcome lands on the `cold` topic.
pub(crate) async fn fold_loop(client: Arc<Client>, session: Arc<Session>, tools: Vec<Value>) {
    loop {
        session.fold_signal.notified().await;
        if !session.folding() {
            continue;
        }
        let outcome = match fold_once(&client, &session, &tools).await {
            Ok(()) => None,
            Err(error) => Some(FoldState::Failed { error }),
        };
        session.finish_fold(outcome);
    }
}

async fn fold_once(client: &Client, session: &Session, tools: &[Value]) -> Result<(), String> {
    let entries = session.snapshot();
    let cut = fold_cut(&entries, FOLD_TAIL_TOKENS)
        .ok_or_else(|| NotFoldable::Nothing.message().to_string())?;
    let turns_before = session.metrics.lock().unwrap().turn_requests;
    // The account as it is written goes to `output` subscribers and nowhere else: the
    // mark below stays its only durable record (docs/summary-head.md).
    let narration =
        crate::narrate::narrate(
            client,
            tools,
            &entries,
            Some(cut),
            |streamed| match streamed {
                Streamed::Text { text, .. } => {
                    session.publish_delta(StreamDelta::NarratorText { text })
                }
                Streamed::Thinking { text, .. } => {
                    session.publish_delta(StreamDelta::NarratorThinking { text })
                }
                Streamed::ToolStart { .. } | Streamed::ToolInput { .. } => {}
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    session.record_request(RequestOrigin::Narrate, narration.usage);
    // A turn that reached the cache while the account was being written (the operator
    // paid the full read meanwhile) makes the fold moot. A keepalive ping does
    // not: on a warm fold it is the cache being kept, not read for a turn.
    if session.metrics.lock().unwrap().turn_requests != turns_before {
        return Err("superseded: a request read the cache while the account was written".into());
    }
    let now = session.snapshot();
    if !crate::consolidate::split_calls(&now, cut).is_empty() {
        return Err("superseded: the log moved across the cut".into());
    }
    let writer = crate::log::recorded_inference(&now)
        .cloned()
        .map(Writer::Narrator)
        .ok_or("the log records no boot identity to name the narrator")?;
    let mark = Mark::boundary(cut, narration.head).written_by(writer);
    session
        .append(session.latest_turn(), Payload::Mark(mark))
        .map_err(|e| format!("appending the fold: {e}"))?;
    Ok(())
}
