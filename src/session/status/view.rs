//! Building the status view (docs/control-api.md): where one session stands, as the one
//! value ([`StatusView`]) the host serves as the `status` state topic and the renderer
//! prints (`render.rs`).
//!
//! Built from [`SessionStatus`] plus the session state each condition needs.

use super::super::cold::ParkReason;
use super::*;
use crate::protocol::state::Cost;
use crate::protocol::status::{
    Activity, Background, Block, CLOSING_CAP, Closing, Condition, Escalated, Footer, HeldCall,
    Park, Reviewer, StatusView,
};
use crate::record::log::{Origin, Spot};

impl Session {
    /// `status` as the view (docs/control-api.md). The status is passed in rather than
    /// read, so a watcher posts the condition it saw, and the host can post the ends
    /// [`Session::status`] never yields.
    pub(crate) fn status_view(&self, id: &str, status: &SessionStatus) -> StatusView {
        let condition = match status {
            SessionStatus::Running => Condition::Working {
                activity: self.activity(),
            },
            SessionStatus::Idle => Condition::Idle {
                closing: self.closing(),
            },
            SessionStatus::Gated {
                call_id,
                name,
                intent,
                ..
            } => Condition::Gated(self.held_call(call_id, name, intent.clone())),
            SessionStatus::Asking { question, n, of } => {
                let handle = self.asking().and_then(|ask| self.handle_of(&ask.call_id));
                Condition::Asking {
                    question: question.clone(),
                    n: *n,
                    of: *of,
                    handle,
                    returned: self.returned_by_ask(),
                }
            }
            SessionStatus::Stalled { message } => Condition::Stalled {
                message: message.clone(),
                held_triage: self.stall().is_some_and(|stall| {
                    matches!(
                        stall.reason,
                        StallReason::TriageUnavailable | StallReason::TriageDefect
                    )
                }),
            },
            SessionStatus::Parked { reason } => Condition::Parked {
                park: match reason {
                    ParkReason::Expired { context_tokens, .. } => Park::Expired {
                        context_tokens: *context_tokens,
                    },
                    ParkReason::Compacted => Park::Compacted,
                },
            },
            SessionStatus::Blocked {
                reason: BlockedReason::PaceHeld,
            } => Condition::Blocked {
                block: Block::PaceHeld,
            },
            SessionStatus::Blocked {
                reason: BlockedReason::ConfigRepair,
            } => {
                let repair = self.config_repair();
                Condition::Blocked {
                    block: Block::ConfigRepair {
                        path: repair.as_ref().map(|repair| repair.path.clone()),
                        parse_error: repair.map(|repair| repair.parse_error),
                    },
                }
            }
            // `Session::status` reads a held triage as the stall it publishes; this arm
            // is a status built by hand.
            SessionStatus::Blocked { .. } => Condition::Stalled {
                message: "a triage is holding the results".to_string(),
                held_triage: true,
            },
            SessionStatus::Ended => Condition::Ended,
            SessionStatus::Failed { error } => Condition::Failed {
                error: error.clone(),
            },
        };
        StatusView {
            id: id.to_string(),
            condition,
            footer: self.footer(),
        }
    }

    /// The session a held call is routed to rather than reported to (docs/permissions.md):
    /// the session its last escalation went to, while that session is hosted; with no
    /// escalation, the live supervisor while the position in force is `supervisor`. `None`
    /// means the operator decides. An escalation overrides the dial, so turning the dial
    /// or moving the pointer does not route an escalated call back.
    pub(crate) fn gate_reviewer(&self, escalations: &[Escalated]) -> Option<String> {
        if let Some(last) = escalations.last() {
            return last.to.clone().filter(|id| self.host_holds(id));
        }
        if self.resolved_review_level() != crate::config::policy::ReviewLevel::Supervisor {
            return None;
        }
        self.supervisor()
    }

    fn held_call(&self, call_id: &str, name: &str, intent: Option<String>) -> HeldCall {
        let (summary, escalations) = {
            let pending = self.pending.lock().unwrap();
            let pending = pending
                .as_ref()
                .filter(|pending| pending.gate.call_id == call_id);
            let escalations = pending.map_or_default(|pending| pending.gate.escalations.clone());
            let summary = pending.and_then(|pending| match &pending.gate.acting_on {
                // A bound approval decides the child's move, so that is what it does.
                Some(bound) => {
                    let leaf = ToolCall::new(&bound.name, call_id, bound.input.clone());
                    let leaf_summary = transcript::call_summary(&leaf)
                        .map_or_default(|summary| format!(" {}", transcript::code_span(&summary)));
                    Some(format!(
                        "{}'s {}{leaf_summary}",
                        bound.session,
                        crate::record::names::tools::display_name(&bound.name)
                    ))
                }
                None => transcript::call_summary(&ToolCall::new(
                    &pending.gate.name,
                    call_id,
                    pending.gate.input.clone(),
                )),
            });
            (summary, escalations)
        };
        let held_because = self
            .latest_spot(&CallId(call_id.to_string()))
            .and_then(|spot| held_because(&spot));
        let reviewer = match self.gate_reviewer(&escalations) {
            Some(id) => Reviewer::Supervisor { id },
            None => Reviewer::Operator,
        };
        HeldCall {
            call_id: call_id.to_string(),
            tool: crate::record::names::tools::display_name(name),
            summary,
            intent,
            handle: self.handle_of(call_id),
            held_because,
            reviewer,
            escalations,
        }
    }

    /// The last turn's closing prose, whole up to [`CLOSING_CAP`] (`push::closing_output`
    /// chooses the entry). `None` when that entry holds no prose.
    fn closing(&self) -> Option<Closing> {
        let log = self.log.lock().unwrap();
        let (seq, output) = push::closing_output(log.entries())?;
        let text: Vec<&str> = output
            .text()
            .into_iter()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect();
        if text.is_empty() {
            return None;
        }
        let whole = text.join("\n\n");
        let cut = match whole
            .char_indices()
            .find(|(i, c)| i + c.len_utf8() > CLOSING_CAP)
        {
            Some((i, _)) => whole[..i].to_string(),
            None => whole.clone(),
        };
        Some(Closing {
            seq,
            text: cut,
            bytes: whole.len(),
            lines: whole.lines().count(),
        })
    }

    /// What a working loop is doing: the open operations, then a retry pause or a pace
    /// delay.
    fn activity(&self) -> Vec<Activity> {
        let retry = self.upstream_retry.get();
        let mut activity = self.operations.lock().unwrap().activity(retry.is_none());
        if let Some(retry) = retry {
            activity.push(Activity::Retrying {
                attempt: retry.attempt,
                budget: Some(retry.budget),
                next_at: Some(retry.next_attempt_at),
            });
        } else if let LoopPhase::Delayed { until } = self.loop_phase() {
            activity.push(Activity::Paced { until });
        }
        activity
    }

    fn footer(&self) -> Footer {
        let context_limit = self
            .capabilities
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|caps| caps.context_length);
        let context_used = self
            .metrics
            .lock()
            .unwrap()
            .last_turn_usage
            .as_ref()
            .map(Usage::prompt_tokens);
        let cost = {
            let cell = self.cost.lock().unwrap();
            cell.endpoint.as_ref().map(|_| Cost {
                usd: cell.total_usd,
                unpriced: cell.unpriced,
            })
        };
        Footer {
            model: self.identity().map(|identity| identity.model),
            context_used,
            context_limit,
            cost,
            background: self
                .process_states()
                .into_iter()
                .filter(|process| process.background)
                .map(|process| Background {
                    id: process.id,
                    command: process.command,
                    since: process.spawned_at.0,
                })
                .collect(),
            queued: self
                .queued_turns()
                .into_iter()
                .map(|turn| turn.source)
                .collect(),
            held: self.pace_snapshot().1,
        }
    }
}

/// Why the spotter held a call, in plain words for whoever decides it
/// (docs/permissions.md). The decision rung's hold is phrased from its recorded answers
/// (`jev::plain_reason`); the prose rung's reason is shown as it wrote it; a fail-closed
/// verdict judged nothing. `Spot::reason` itself is unchanged, and remains what the
/// session, the TUI gate and the audits read.
pub(crate) fn held_because(spot: &Spot) -> Option<String> {
    if spot.origin == Origin::Fallback {
        return Some("its reviewer could not judge it".to_string());
    }
    let written = || Some(spot.reason.trim_end().to_string());
    match &spot.axes {
        // Answers that fold to a clear under the table in force (an entry recorded under
        // another fold) still held this call; its own reason says why.
        Some(axes) => crate::curation::spotter::jev::plain_reason(axes).or_else(written),
        None => written(),
    }
}

#[cfg(test)]
mod test;
