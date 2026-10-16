//! What a session is doing, as another session hears it (docs/tools.md).
//!
//! A subscriber to a session receives its *status* — a current condition, never
//! an event stream — as an inbox notification on the slot `session:<id>`. The
//! statuses a subscriber hears are the decision points: a gate, a question, a turn
//! completing into idle, a stall, a park or a block, and the end. `Running` is
//! observed to detect the transitions but never posted.

use super::*;

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

    /// The notification body a subscriber reads for `status` (docs/tools.md):
    /// what the session is doing, the call to reach for, and the position in its
    /// log so the subscriber can pull the transcript from where it last read.
    pub(crate) fn status_body(&self, id: &str, status: &SessionStatus) -> String {
        let at = match self.last_committed_seq() {
            Some(seq) => format!("at seq {seq}"),
            None => "with an empty log".to_string(),
        };
        match status {
            SessionStatus::Running => format!("{id} is running ({at})."),
            SessionStatus::Idle => {
                let said = super::push::settle_body(self.log.lock().unwrap().entries());
                let mut body = format!(
                    "{id} finished its turn and is idle {at}; sessions__transcript shows what \
                     it did."
                );
                if !said.is_empty() {
                    body.push_str(&format!(" It said: {said}"));
                }
                body
            }
            SessionStatus::Gated {
                call_id,
                name,
                intent,
                reason,
            } => {
                let display = crate::mcp::display_name(name);
                let mut body = format!("{id} is gated on {display}");
                if let Some(intent) = intent {
                    body.push_str(&format!(" — {intent}"));
                }
                body.push_str(&format!(" ({at})."));
                if let Some(reason) = reason {
                    body.push_str(&format!(" Held because: {}", reason.trim_end()));
                    if !reason.trim_end().ends_with('.') {
                        body.push('.');
                    }
                }
                body.push_str(&format!(
                    " Nothing runs until call_id {call_id} is resolved with sessions__approve \
                     (approve, abort, or redirect); sessions__state shows the held call."
                ));
                body
            }
            SessionStatus::Asking { question, n, of } => {
                let count = if *of > 1 {
                    format!(" (question {n} of {of})")
                } else {
                    String::new()
                };
                format!(
                    "{id} is asking a question{count} ({at}): {}\nNothing runs until it is \
                     answered; a turn submitted with sessions__turn is the reply.",
                    question.trim_end()
                )
            }
            SessionStatus::Stalled { message } => format!("{id} stalled {at}: {message}"),
            SessionStatus::Parked {
                reason: super::cold::ParkReason::Compacted,
            } => format!(
                "{id} is parked mid-turn ({at}): its history was summarized while it waited \
                 on a cold cache. A sessions__turn or a continue resumes it."
            ),
            SessionStatus::Parked {
                reason: super::cold::ParkReason::Expired { context_tokens, .. },
            } => {
                let cost = match context_tokens {
                    Some(tokens) => format!("~{tokens} tokens uncached"),
                    None => "the whole context uncached".to_string(),
                };
                format!(
                    "{id} is parked mid-turn ({at}): its prompt cache is believed expired, so \
                     continuing reads {cost}. Nothing runs until a sessions__turn, a continue or a decision \
                     releases it; a compaction summarizes the history instead."
                )
            }
            SessionStatus::Blocked {
                reason: BlockedReason::PaceHeld,
            } => format!(
                "{id} is held by its operator's pace ({at}). Nothing runs until the hold is \
                 released or a turn is sent."
            ),
            SessionStatus::Blocked { .. } => format!(
                "{id} is blocked ({at}): its config file does not parse and could not be \
                 restored. Nothing runs until a human repairs the file; sessions__state names it."
            ),
            SessionStatus::Ended => format!("{id} ended ({at}); its log stays on disk."),
            SessionStatus::Failed { error } => {
                format!("{id} ended with an error ({at}): {error}")
            }
        }
    }

    /// The same status body, addressed to the session the gate is *routed* to under
    /// the `supervisor` review position (docs/permissions.md). The added sentence names
    /// the recipient as the reviewer and names the dial that hands the child back to
    /// the operator, so declining is a move the reader can make rather than a silence.
    pub(crate) fn supervisor_gate_body(&self, id: &str, status: &SessionStatus) -> String {
        let mut body = self.status_body(id, status);
        if matches!(status, SessionStatus::Gated { .. }) {
            body.push_str(&format!(
                " You are its supervisor: answer with sessions__approve, or hand \
                 {id} back to the operator with sessions__review_level (spotter_human), \
                 which re-routes this gate rather than dropping it."
            ));
        }
        body
    }
}

/// The inbox slot a session's status posts on: one slot per session, so a
/// subscriber's pending inbox holds the latest condition rather than a pile.
pub(crate) fn status_slot(id: &str) -> String {
    format!("session:{id}")
}
