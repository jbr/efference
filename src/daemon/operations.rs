//! The `current_operations` state topic's source (docs/control-api.md): what the
//! loop is waiting on right now, folded from the log.
//!
//! Two kinds of thing are open at a time. **Requests**: every upstream request that
//! has a `Sent` stage and no closing one (`RequestStage`, docs/log-and-projection.md),
//! each with the stage it is in and when that stage began. **Tools**: calls with a
//! `CallStarted` entry and no result yet, with the tool name, `intent`, and (for
//! `bash`) `command` the assistant entry carried. Where the loop itself waits (a gate, an
//! `ask`, a park, a hold) is the `loop_phase` topic, not repeated here.
//!
//! The same fold keeps the **compaction** (docs/summary.md), the third kind of open
//! operation: a `CompactionRequested` entry opens one, waiting until its summarizing
//! request is sent and writing after; the compaction's `Summary` closes it. A
//! `CompactionFailed` closes it too and leaves its failure standing — the
//! `compaction_failure` topic, not an operation — until a turn's request goes out or
//! another compaction is asked for. A
//! failure is in the log, so it survives a restart; a compaction open when the host
//! stopped does not, because the process writing it is gone.
//!
//! The fold is kept incrementally: [`Session::append`] feeds each new entry to
//! [`Operations::observe`] under its own lock, and a session opened from an existing
//! log builds the same state with [`Operations::fold`]. Re-folding the whole log on
//! every recompute would cost the log's length at every append, and the topic is
//! recomputed on each one. A `Resumed` entry closes everything open before it: those
//! requests and calls belonged to the previous host process, which is gone.

use crate::log::Telemetry;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use serde_json::Value;

use super::Session;
use crate::api::{StageRecorder, StageSink};
use crate::log::{
    CallId, LogEntry, MadeBy, Payload, RequestOrigin, RequestStage, RequestStageKind, Seq, Summary,
    Timestamp,
};
use crate::persist::Log;
use crate::protocol::state::{
    CurrentOperations, OpenCompaction, OpenRequest, OpenStage, RunningTool,
};
use crate::protocol::status::Activity;

/// What the assistant entry said about one issued call.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IssuedCall {
    tool: String,
    intent: Option<String>,
    command: Option<String>,
}

/// Where the compaction asked for last stands (docs/summary.md), folded from its
/// entries. `since` and `at` are the instants of the entries that moved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompactionState {
    /// Accepted (`CompactionRequested`); its summarizing request not yet sent.
    Waiting { warm: bool, cut: Seq, since: u64 },
    /// Its summarizing request has been sent; no summary or failure yet.
    Writing { warm: bool, cut: Seq, since: u64 },
    /// It ended without a summary (`CompactionFailed`).
    Failed { error: String, at: u64 },
}

impl CompactionState {
    /// Whether the compaction is still to land: waiting or writing.
    pub(crate) fn open(&self) -> bool {
        !matches!(self, Self::Failed { .. })
    }
}

/// The open requests and running tools, as of the last entry observed: the
/// `current_operations` topic's [`OpenRequest`]s and [`RunningTool`]s, kept as they are
/// published.
#[derive(Debug, Default)]
pub(crate) struct Operations {
    requests: Vec<OpenRequest>,
    tools: Vec<RunningTool>,
    /// The compaction open now, or the last one's failure.
    compaction: Option<CompactionState>,
    /// The latest completion's calls by id, so `CallStarted` can name the tool.
    issued: HashMap<CallId, IssuedCall>,
    /// The next request id to mint: past every id the log has used.
    next_request: u64,
}

impl Operations {
    /// The state a log of `entries` leaves.
    pub(crate) fn fold(entries: &[LogEntry]) -> Self {
        let mut operations = Self::default();
        for entry in entries {
            operations.observe(entry.at, &entry.payload);
        }
        operations
    }

    /// A fresh request id.
    pub(crate) fn mint(&mut self) -> u64 {
        let id = self.next_request;
        self.next_request += 1;
        id
    }

    /// Fold one appended entry in.
    pub(crate) fn observe(&mut self, at: Timestamp, payload: &Payload) {
        let at = at.0;
        match payload {
            Payload::Telemetry(Telemetry::RequestStage(stage)) => self.observe_stage(at, stage),
            Payload::Assistant { output, .. } => {
                // A new completion means the previous batch is over, finished or not.
                self.tools.clear();
                self.issued = output
                    .tool_calls()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|call| {
                        let intent = call.intent().map(str::to_string);
                        let command = (call.name == crate::bash::NAME)
                            .then(|| call.input.get("command").and_then(Value::as_str))
                            .flatten()
                            .map(str::to_string);
                        let issued = IssuedCall {
                            tool: call.name,
                            intent,
                            command,
                        };
                        (call.id, issued)
                    })
                    .collect();
            }
            Payload::User(_) => self.tools.clear(),
            Payload::Telemetry(Telemetry::CallStarted { call_id }) => {
                if let Some(issued) = self.issued.get(call_id) {
                    self.tools.push(RunningTool {
                        call_id: call_id.clone(),
                        tool: issued.tool.clone(),
                        intent: issued.intent.clone(),
                        command: issued.command.clone(),
                        since: at,
                    });
                }
            }
            Payload::ToolResult { call_id, .. } => {
                self.tools.retain(|tool| tool.call_id != *call_id);
            }
            Payload::Resumed(_) => {
                self.requests.clear();
                self.tools.clear();
                self.issued.clear();
                // The process writing an open compaction is gone; a failure stands.
                if self.compaction.as_ref().is_some_and(CompactionState::open) {
                    self.compaction = None;
                }
            }
            Payload::Telemetry(Telemetry::CompactionRequested { warm, cut }) => {
                // A second request while one is open is the same compaction.
                if !self.compaction.as_ref().is_some_and(CompactionState::open) {
                    self.compaction = Some(CompactionState::Waiting {
                        warm: *warm,
                        cut: *cut,
                        since: at,
                    });
                }
            }
            Payload::Telemetry(Telemetry::CompactionFailed { error }) => {
                self.compaction = Some(CompactionState::Failed {
                    error: error.clone(),
                    at,
                });
            }
            Payload::Summary(Summary {
                made_by: MadeBy::Compaction(_),
                ..
            }) => self.compaction = None,
            _ => {}
        }
    }

    /// The compaction open now, or the last one's failure.
    pub(crate) fn compaction(&self) -> Option<&CompactionState> {
        self.compaction.as_ref()
    }

    fn observe_stage(&mut self, at: u64, stage: &RequestStage) {
        self.next_request = self.next_request.max(stage.request + 1);
        if stage.stage == RequestStageKind::Sent {
            match (&self.compaction, stage.origin) {
                (Some(CompactionState::Waiting { warm, cut, .. }), RequestOrigin::Compaction) => {
                    self.compaction = Some(CompactionState::Writing {
                        warm: *warm,
                        cut: *cut,
                        since: at,
                    });
                }
                // A turn going out makes the last failure stale: the session moved on.
                (Some(CompactionState::Failed { .. }), RequestOrigin::Turn) => {
                    self.compaction = None;
                }
                _ => {}
            }
        }
        if stage.stage.closes() {
            self.requests.retain(|open| open.request != stage.request);
            return;
        }
        let next = match &stage.stage {
            RequestStageKind::Sent => OpenStage::Sent,
            RequestStageKind::AttemptFailed { attempt, status } => OpenStage::Retrying {
                attempt: *attempt,
                status: *status,
            },
            RequestStageKind::FirstByte => OpenStage::Streaming,
            RequestStageKind::Ended | RequestStageKind::Failed { .. } => return,
        };
        match self
            .requests
            .iter_mut()
            .find(|open| open.request == stage.request)
        {
            Some(open) => {
                open.stage = next;
                open.since = at;
            }
            // Only `Sent` opens a request: any other stage naming one that is not
            // open belongs to a request the log already closed or a restart dropped.
            None if next == OpenStage::Sent => self.requests.push(OpenRequest {
                request: stage.request,
                origin: stage.origin,
                stage: next,
                since: at,
            }),
            None => {}
        }
    }

    /// The topic's value: `None` when nothing is open, else the requests, the tools, and
    /// the compaction, `compaction` being `None` unless one is waiting or writing.
    pub(crate) fn view(&self) -> Option<CurrentOperations> {
        let compaction = match self.compaction {
            Some(CompactionState::Waiting { warm, cut, since }) => {
                Some(OpenCompaction::Waiting { warm, cut, since })
            }
            Some(CompactionState::Writing { warm, cut, since }) => {
                Some(OpenCompaction::Writing { warm, cut, since })
            }
            Some(CompactionState::Failed { .. }) | None => None,
        };
        if self.requests.is_empty() && self.tools.is_empty() && compaction.is_none() {
            return None;
        }
        Some(CurrentOperations {
            requests: self.requests.clone(),
            tools: self.tools.clone(),
            compaction,
        })
    }
}

impl Operations {
    /// What is open, as the status names a working loop's activity (`status/view.rs`):
    /// the running tools, then the turn's and the triage's requests, then a compaction.
    /// A request of any other origin (the spotter, a title, a keepalive) is the
    /// harness's, not the session's work, and is left out. `retrying` keeps a turn
    /// request the transport is retrying; the caller drops it when the retry cell
    /// already reports the retry with its budget.
    pub(crate) fn activity(&self, retrying: bool) -> Vec<Activity> {
        let tools = self.tools.iter().map(|tool| Activity::Tool {
            tool: crate::mcp::display_name(&tool.tool),
            command: tool.command.clone(),
            since: tool.since,
        });
        let requests =
            self.requests
                .iter()
                .filter_map(|open| match (open.origin, &open.stage) {
                    (RequestOrigin::Turn, OpenStage::Retrying { attempt, .. }) => retrying
                        .then_some(Activity::Retrying {
                            attempt: *attempt,
                            budget: None,
                            next_at: None,
                        }),
                    (RequestOrigin::Turn, _) => Some(Activity::Thinking { since: open.since }),
                    (RequestOrigin::Triage, _) => Some(Activity::Triaging { since: open.since }),
                    _ => None,
                });
        let compaction = match &self.compaction {
            Some(
                CompactionState::Waiting { since, .. } | CompactionState::Writing { since, .. },
            ) => Some(Activity::Compacting { since: *since }),
            _ => None,
        };
        tools.chain(requests).chain(compaction).collect()
    }
}

impl Session {
    /// Fold the entry just appended into the operations view. Called with the log
    /// lock held, so the view sees entries in log order.
    pub(super) fn observe_operations(&self, log: &Log) {
        if let Some(entry) = log.entries().last() {
            self.operations
                .lock()
                .unwrap()
                .observe(entry.at, &entry.payload);
        }
    }

    /// The `current_operations` topic (docs/control-api.md).
    pub(super) fn current_operations(&self) -> Option<CurrentOperations> {
        self.operations.lock().unwrap().view()
    }

    /// The sink a client reports its request stages to: ids minted from this
    /// session's counter, stages appended to its log. Holds the session weakly, so
    /// a client that outlives the session records nothing.
    pub fn stage_sink(self: &Arc<Self>) -> StageSink {
        Arc::new(SessionStages(Arc::downgrade(self)))
    }
}

struct SessionStages(Weak<Session>);

impl StageRecorder for SessionStages {
    fn mint(&self) -> u64 {
        match self.0.upgrade() {
            Some(session) => session.operations.lock().unwrap().mint(),
            None => 0,
        }
    }

    fn record(&self, stage: RequestStage) {
        let Some(session) = self.0.upgrade() else {
            return;
        };
        // Telemetry: a failed write is reported and dropped rather than failing the
        // request it describes, as for process events.
        let turn = session.latest_turn();
        if let Err(e) = session.append(turn, Payload::Telemetry(Telemetry::RequestStage(stage))) {
            log::warn!("request stage: append failed: {e}");
        }
    }
}

#[cfg(test)]
mod test;
