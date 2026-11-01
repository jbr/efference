//! What is in flight beside the loop, and how the last compaction ended
//! (docs/control-api.md): the `current_operations` and `compaction_failure` topics. The
//! host folds both from the log as entries append (`session/operations.rs`).

use serde::{Deserialize, Serialize};

use crate::record::log::{CallId, RequestOrigin, Seq};

/// The `current_operations` topic: the upstream requests open now, the tool calls
/// executing, and a compaction waiting or writing. The topic is `null` rather than empty
/// when nothing is open; where the loop itself waits is the `loop_phase` topic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CurrentOperations {
    /// Open requests, in the order they were sent.
    pub requests: Vec<OpenRequest>,
    /// Calls dispatched and not yet resulted.
    pub tools: Vec<RunningTool>,
    /// The compaction asked for, while it is still to land.
    pub compaction: Option<OpenCompaction>,
}

/// A request that has been sent and not closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct OpenRequest {
    /// The id the request's log entries share (docs/log-and-projection.md).
    pub request: u64,
    pub origin: RequestOrigin,
    #[serde(flatten)]
    pub stage: OpenStage,
    /// When this stage began, absolute Unix millis.
    pub since: u64,
}

/// Where an open request is, published as its `stage`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum OpenStage {
    /// Handed to the transport; no stream event yet.
    Sent,
    /// An attempt was refused and the transport is trying again: `attempt` is the one
    /// that failed, `status` the refusal's.
    Retrying { attempt: u32, status: u16 },
    /// The first stream event arrived.
    Streaming,
}

/// A call dispatched and not yet resulted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct RunningTool {
    pub call_id: CallId,
    /// The tool's wire name.
    pub tool: String,
    /// The call's one-line intent; `null` when it gave none.
    pub intent: Option<String>,
    /// The shell command, for a `bash` call; absent for any other tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// When the call started, absolute Unix millis.
    pub since: u64,
}

/// A compaction still to land (docs/summary.md), published as its `state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OpenCompaction {
    /// Accepted; its summarizing request not yet sent.
    Waiting {
        /// Asked of a session whose cache was believed live.
        warm: bool,
        /// The last seq the summary covers.
        cut: Seq,
        /// When this state began, absolute Unix millis.
        since: u64,
    },
    /// Its summarizing request has been sent.
    Writing { warm: bool, cut: Seq, since: u64 },
}

/// The `compaction_failure` topic: the last compaction ended without a summary
/// (docs/summary.md). It stands until a turn's request goes out or another compaction is
/// asked for, and is folded from the log, so it survives a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CompactionFailure {
    pub error: String,
    /// When it failed, absolute Unix millis.
    pub at: u64,
}
