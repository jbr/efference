//! What a session is doing, as another session hears it (docs/tools.md).
//!
//! A subscriber to a session receives its *status* — a current condition, never
//! an event stream — as an inbox notification on the slot `session:<id>`. The
//! statuses a subscriber hears are the decision points: a gate, a turn completing
//! into idle, a stall, and the end. `Running` is observed to detect the
//! transitions but never posted.

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
    /// before it reads the expired cache, or for a fold (backlog: cold-session.md); after a fold it
    /// waits for the next turn or continue. The `park` topic.
    Parked { reason: super::cold::ParkReason },
    /// The runtime returned cleanly.
    Ended,
    /// The runtime returned with an error.
    Failed { error: String },
}

impl Session {
    /// The session's status now. A gate outranks a stall — nothing runs while a
    /// call is held — and a stall outranks the running flag, since the loop sets
    /// the stall before it parks. A cold park ranks after those three and before the
    /// running flag, which stays set while the loop waits.
    pub(crate) fn status(&self) -> SessionStatus {
        if let Some(pending) = self.pending.lock().unwrap().as_ref() {
            let intent = pending
                .input
                .get(crate::tools::INTENT)
                .and_then(Value::as_str)
                .map(str::to_string);
            return SessionStatus::Gated {
                call_id: pending.call_id.clone(),
                name: pending.name.clone(),
                intent,
                reason: pending.reason.clone(),
            };
        }
        // A pending question outranks a stall for the same reason a gate does: the loop
        // is parked on the operator, and nothing runs until they answer.
        if let Some(ask) = self.asking() {
            return SessionStatus::Asking {
                question: ask.question,
                n: ask.n,
                of: ask.of,
            };
        }
        if let Some(stall) = self.stall() {
            return SessionStatus::Stalled {
                message: stall.message,
            };
        }
        if let Some(reason) = self.park_reason() {
            return SessionStatus::Parked { reason };
        }
        if self.running.load(Ordering::SeqCst) {
            SessionStatus::Running
        } else {
            SessionStatus::Idle
        }
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
                reason: super::cold::ParkReason::Folded,
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
                     releases it; a fold summarizes the history instead."
                )
            }
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
