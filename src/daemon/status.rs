//! Where a session stands (docs/control-api.md, docs/tools.md).
//!
//! A session's *status* is a current condition, never an event stream. The host reads
//! it off the loop phase ([`Session::status`]), builds it into one view with what each
//! condition needs (`view.rs`), and prints that view with one renderer (`render.rs`).
//! The view is the `status` state topic; the rendering is what a subscriber's inbox
//! receives on the slot `session:<id>`. The statuses a subscriber hears are the
//! decision points: a gate, a question, a turn completing into idle, a stall, a park or
//! a block, and the end. `Running` is observed to detect the transitions but never
//! posted.

use super::*;

mod moves;
mod render;
mod view;

pub(crate) use render::{Reader, Relation, render};
pub(crate) use view::{Activity, Condition, Footer, Reviewer, StatusView};

/// One session's condition at a decision point, as read off its control state.
/// `Ended` and `Failed` are the host's to report once the runtime has returned;
/// [`Session::status`] never yields them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    /// Executing a turn.
    Running,
    /// Waiting with nothing queued.
    Idle,
    /// Holding a call for a decision (docs/permissions.md).
    Gated {
        call_id: String,
        /// The held tool's wire name.
        name: String,
        intent: Option<String>,
        /// The spotter's reason for routing it to the gate, when one preceded it.
        reason: Option<String>,
    },
    /// Waiting on the operator's reply to an `ask` (docs/process-monitor.md): question `n` of
    /// `of` from one completion.
    Asking {
        question: String,
        n: usize,
        of: usize,
    },
    /// The last trajectory gave up (docs/transport.md).
    Stalled { message: String },
    /// The loop stopped at its cold check mid-turn and waits for a person to act
    /// before it reads the expired cache, or for a compaction (backlog: cold-session.md); after a compaction it
    /// waits for the next turn or continue. The `cold_parked` loop phase.
    Parked { reason: super::cold::ParkReason },
    /// The loop is stopped until someone outside acts, for a reason that is neither a
    /// held triage (which is [`Stalled`](Self::Stalled)) nor a park: the config file
    /// needs repair, or the operator's pace is held. The `blocked` loop phase.
    Blocked { reason: BlockedReason },
    /// The runtime returned cleanly.
    Ended,
    /// The runtime returned with an error.
    Failed { error: String },
}

impl Session {
    /// The session's status now, read off the loop phase (docs/control-api.md). A triage
    /// held without a verdict is the stall it publishes, and an idle loop whose last
    /// trajectory gave up is stalled; every phase that is neither waiting nor idle is
    /// running.
    pub(crate) fn status(&self) -> SessionStatus {
        match self.loop_phase() {
            LoopPhase::Requesting | LoopPhase::Executing | LoopPhase::Delayed { .. } => {
                SessionStatus::Running
            }
            LoopPhase::Idle => match self.stall() {
                Some(stall) => SessionStatus::Stalled {
                    message: stall.message,
                },
                None => SessionStatus::Idle,
            },
            LoopPhase::Gated => self.gated_status().unwrap_or(SessionStatus::Running),
            LoopPhase::Asking => {
                self.asking()
                    .map_or(SessionStatus::Running, |ask| SessionStatus::Asking {
                        question: ask.question,
                        n: ask.n,
                        of: ask.of,
                    })
            }
            LoopPhase::ColdParked => {
                self.park_reason()
                    .map_or(SessionStatus::Running, |reason| SessionStatus::Parked {
                        reason,
                    })
            }
            LoopPhase::Blocked {
                reason: BlockedReason::TriageUnavailable | BlockedReason::TriageDefect,
            } => match self.stall() {
                Some(stall) => SessionStatus::Stalled {
                    message: stall.message,
                },
                None => SessionStatus::Running,
            },
            LoopPhase::Blocked { reason } => SessionStatus::Blocked { reason },
        }
    }

    /// The pending gate as a status, or `None` when the gate has already been answered.
    fn gated_status(&self) -> Option<SessionStatus> {
        let pending = self.pending.lock().unwrap();
        let pending = pending.as_ref()?;
        let intent = pending
            .input
            .get(crate::tools::INTENT)
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(SessionStatus::Gated {
            call_id: pending.call_id.clone(),
            name: pending.name.clone(),
            intent,
            reason: pending.reason.clone(),
        })
    }

    /// How many trajectories have ended into idle — what tells a status watcher that an
    /// idle followed a turn (`subscriptions.rs`).
    pub(crate) fn turns_ended(&self) -> u64 {
        self.turns_ended.load(Ordering::SeqCst)
    }
}

/// The inbox slot a session's status posts on: one slot per session, so a
/// subscriber's pending inbox holds the latest condition rather than a pile.
pub(crate) fn status_slot(id: &str) -> String {
    format!("session:{id}")
}
