//! The status view (docs/control-api.md): where one session stands, as one value the
//! host builds and the renderer prints (`render.rs`).
//!
//! Built from [`SessionStatus`] plus the session state each condition needs — the held
//! call's handle and the reason it was held, the question's place, the closing message,
//! what a working loop is doing — and served as the `status` state topic. Instants are
//! absolute Unix millis, so the topic changes only at a transition; the renderer
//! subtracts them from its own clock.

use super::super::cold::ParkReason;
use super::super::starting::Stage;
use super::*;
use crate::log::{Origin, Spot};

/// The most of a closing message the view carries. Past it the message is cut on a char
/// boundary and the view says how much was left out.
pub(crate) const CLOSING_CAP: usize = 4096;

/// One session's status, as every surface renders it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StatusView {
    pub(crate) id: String,
    pub(crate) condition: Condition,
    pub(crate) footer: Footer,
}

/// The condition, and what each one carries for its reader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Condition {
    /// Being prepared (`starting.rs`).
    Starting {
        stage: Stage,
        /// The MCP server whose authorization preparation waits on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorizing: Option<String>,
        since: u64,
    },
    /// Preparation ended without a session.
    FailedToStart { reason: String },
    /// Executing a turn: what is open now, possibly nothing.
    Working { activity: Vec<Activity> },
    /// Holding a call for a decision.
    Gated(HeldCall),
    /// Waiting on the answer to a question, `n` of `of`.
    Asking {
        question: String,
        n: usize,
        of: usize,
        /// The asking call's transcript handle.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        /// The queued messages the question returned undelivered (docs/process-monitor.md),
        /// so each sender can be told its own.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        returned: Vec<Returned>,
    },
    /// Its turn is done; what it said last.
    Idle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        closing: Option<Closing>,
    },
    /// The last trajectory gave up. `held_triage` is a triage holding the batch's results,
    /// which the operator's continue retries rather than a message.
    Stalled { message: String, held_triage: bool },
    /// Stopped at its cold check.
    Parked { park: Park },
    /// Stopped until someone outside the session's drivers acts.
    Blocked { block: Block },
    /// The runtime returned cleanly.
    Ended,
    /// The runtime returned with an error.
    Failed { error: String },
}

/// A held call, as its decider reads it.
/// A queued message a question returned undelivered: who sent it and what it said.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Returned {
    /// The turn's `source`: `None` for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HeldCall {
    /// The provider's id for the call, which the approval route takes; `decide` maps
    /// the handle to it.
    pub(crate) call_id: String,
    /// The tool's display name (`fs.list`).
    pub(crate) tool: String,
    /// What it does: a command, or the arguments (`transcript::call_summary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) intent: Option<String>,
    /// The transcript handle, `seq` or `seq.n`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) handle: Option<String>,
    /// Why it was held, in plain words ([`held_because`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) held_because: Option<String>,
    pub(crate) reviewer: Reviewer,
}

/// Who decides a held call (docs/permissions.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Reviewer {
    Operator,
    /// The supervising session the gate is routed to.
    Supervisor {
        id: String,
    },
}

/// A turn's closing message, cut at [`CLOSING_CAP`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Closing {
    /// The entry it was read from.
    pub(crate) seq: Seq,
    pub(crate) text: String,
    /// The whole message's size and line count, so a cut can say how much it left out.
    pub(crate) bytes: usize,
    pub(crate) lines: usize,
}

/// Why a loop is parked at its cold check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Park {
    /// The cache is believed expired; continuing reads `context_tokens` uncached.
    Expired {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_tokens: Option<u64>,
    },
    /// A compaction summarized the history while it waited.
    Compacted,
}

/// What a blocked loop waits on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Block {
    /// The operator's pace is held.
    PaceHeld,
    /// The config file does not parse and could not be restored.
    ConfigRepair {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Why it did not parse; a read of the status prints it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parse_error: Option<String>,
    },
}

/// One thing a working loop is doing (`operations.rs`, the retry cell, the pace).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Activity {
    /// A tool call executing.
    Tool {
        tool: String,
        /// A `bash` call's command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        since: u64,
    },
    /// A turn's request in flight.
    Thinking { since: u64 },
    /// A triage's request in flight.
    Triaging { since: u64 },
    /// A compaction waiting or writing.
    Compacting { since: u64 },
    /// A failed request being tried again: `attempt` is the one that failed, of
    /// `budget` when the loop counts them, the next at `next_at`.
    Retrying {
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_at: Option<u64>,
    },
    /// The operator's pace delay between steps, ending at `until`.
    Paced { until: u64 },
}

/// What a read of the status carries below the condition; subscription notifications
/// leave it out (`render.rs`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Footer {
    /// The main model's native id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    /// The last turn's prompt, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_used: Option<u64>,
    /// The model's context window, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_limit: Option<u64>,
    /// The session's cost, where the provider prices requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cost: Option<Cost>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) background: Vec<Background>,
    /// Each queued turn's `source`, in submission order: `None` for the operator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) queued: Vec<Option<String>>,
    /// The operator holds the session: nothing queued runs until the hold is released
    /// (docs/control-channel.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) held: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Cost {
    pub(crate) usd: f64,
    /// Requests that could not be priced, which make `usd` an undercount.
    pub(crate) unpriced: u32,
}

/// A background process the session holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Background {
    pub(crate) id: u64,
    pub(crate) command: String,
    pub(crate) since: u64,
}

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

    /// The `status` topic (docs/control-api.md).
    pub(in crate::daemon) fn status_json(&self) -> Value {
        let id = self
            .identity()
            .map(|identity| identity.id)
            .unwrap_or_default();
        serde_json::to_value(self.status_view(&id, &self.status())).unwrap_or(Value::Null)
    }

    /// The session a held call is routed to rather than reported to: the live supervisor,
    /// while the position in force is `supervisor` (docs/permissions.md). `None` means the
    /// operator decides.
    fn routed_reviewer(&self) -> Option<String> {
        if self.resolved_review_level() != crate::policy::ReviewLevel::Supervisor {
            return None;
        }
        self.supervisor()
    }

    fn held_call(&self, call_id: &str, name: &str, intent: Option<String>) -> HeldCall {
        let summary = {
            let pending = self.pending.lock().unwrap();
            pending
                .as_ref()
                .filter(|pending| pending.call_id == call_id)
                .and_then(|pending| match &pending.acting_on {
                    // A bound approval decides the child's move, so that is what it does.
                    Some(bound) => {
                        let leaf = ToolCall::new(&bound.name, call_id, bound.input.clone());
                        let leaf_summary = super::super::transcript::call_summary(&leaf)
                            .map(|summary| format!(" `{summary}`"))
                            .unwrap_or_default();
                        Some(format!(
                            "{}'s {}{leaf_summary}",
                            bound.session,
                            crate::mcp::display_name(&bound.name)
                        ))
                    }
                    None => super::super::transcript::call_summary(&ToolCall::new(
                        &pending.name,
                        call_id,
                        pending.input.clone(),
                    )),
                })
        };
        let held_because = self
            .latest_spot(&CallId(call_id.to_string()))
            .and_then(|spot| held_because(&spot));
        let reviewer = match self.routed_reviewer() {
            Some(id) => Reviewer::Supervisor { id },
            None => Reviewer::Operator,
        };
        HeldCall {
            call_id: call_id.to_string(),
            tool: crate::mcp::display_name(name),
            summary,
            intent,
            handle: self.handle_of(call_id),
            held_because,
            reviewer,
        }
    }

    /// The last turn's closing prose, whole up to [`CLOSING_CAP`] (`push::closing_output`
    /// chooses the entry). `None` when that entry holds no prose.
    fn closing(&self) -> Option<Closing> {
        let log = self.log.lock().unwrap();
        let (seq, output) = super::super::push::closing_output(log.entries())?;
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
        Some(axes) => crate::spotter::jev::plain_reason(axes).or_else(written),
        None => written(),
    }
}

#[cfg(test)]
mod test;
