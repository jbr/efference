//! The `current_operations` state topic's source (docs/control-api.md): what the
//! loop is waiting on right now, folded from the log.
//!
//! Two kinds of thing are open at a time. **Requests**: every upstream request that
//! has a `Sent` stage and no closing one (`RequestStage`, docs/log-and-projection.md),
//! each with the stage it is in and when that stage began. **Tools**: calls with a
//! `CallStarted` entry and no result yet, with the tool name, `intent`, and (for
//! `bash`) `command` the assistant entry carried. Parked waits (a gate, an `ask`, a park, a stall) have
//! their own topics and are not repeated here.
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

use serde_json::{Value, json};

use super::Session;
use crate::api::{StageRecorder, StageSink};
use crate::log::{
    CallId, LogEntry, Payload, RequestOrigin, RequestStage, RequestStageKind, Timestamp,
};
use crate::persist::Log;

/// A request that has been sent and not closed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenRequest {
    request: u64,
    origin: RequestOrigin,
    stage: OpenStage,
    /// When this stage began (the stage entry's `at`).
    since: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OpenStage {
    /// Handed to the transport; no stream event yet.
    Sent,
    /// An attempt was refused and the transport is trying again.
    Retrying { attempt: u32, status: u16 },
    /// The first stream event arrived.
    Streaming,
}

/// A call dispatched and not yet resulted.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunningTool {
    call_id: CallId,
    tool: String,
    intent: Option<String>,
    /// The shell command, for a `bash` call; `None` for any other tool.
    command: Option<String>,
    since: u64,
}

/// What the assistant entry said about one issued call.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IssuedCall {
    tool: String,
    intent: Option<String>,
    command: Option<String>,
}

/// The open requests and running tools, as of the last entry observed.
#[derive(Debug, Default)]
pub(crate) struct Operations {
    requests: Vec<OpenRequest>,
    tools: Vec<RunningTool>,
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
            }
            _ => {}
        }
    }

    fn observe_stage(&mut self, at: u64, stage: &RequestStage) {
        self.next_request = self.next_request.max(stage.request + 1);
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

    /// The topic's value: `null` when nothing is open, else `{requests, tools}`.
    pub(crate) fn json(&self) -> Value {
        if self.requests.is_empty() && self.tools.is_empty() {
            return Value::Null;
        }
        let requests: Vec<Value> = self
            .requests
            .iter()
            .map(|open| {
                let mut value = json!({
                    "request": open.request,
                    "origin": open.origin,
                    "since": open.since,
                });
                let stage = match &open.stage {
                    OpenStage::Sent => "sent",
                    OpenStage::Streaming => "streaming",
                    OpenStage::Retrying { attempt, status } => {
                        value["attempt"] = json!(attempt);
                        value["status"] = json!(status);
                        "retrying"
                    }
                };
                value["stage"] = json!(stage);
                value
            })
            .collect();
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                let mut value = json!({
                    "call_id": tool.call_id,
                    "tool": tool.tool,
                    "intent": tool.intent,
                    "since": tool.since,
                });
                if let Some(command) = &tool.command {
                    value["command"] = json!(command);
                }
                value
            })
            .collect();
        json!({ "requests": requests, "tools": tools })
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
    pub(super) fn current_operations_json(&self) -> Value {
        self.operations.lock().unwrap().json()
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
