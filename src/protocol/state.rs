//! `GET /state` and its level-triggered `state` frames (docs/control-api.md): one value
//! per state topic, each the whole of what the topic says now, `null` when it is clear.
//!
//! The host builds every topic from its own state (`daemon/views.rs`) and publishes each
//! one that moved (`daemon/topics.rs`); `GET /state` is [`StateTopics`], and a client
//! that folds the frames holds the same values.
//!
//! The channel carries every topic through `serde_json::Value`, which this crate builds
//! without `preserve_order`, so the keys of each object come out sorted, at every depth,
//! whatever order a type here declares its fields in. The bytes are pinned
//! (`daemon/test/topic_bytes.rs`).

use serde::{Deserialize, Serialize};

use crate::api::{UpstreamFailure, UpstreamRetry};
use crate::inference::InferenceIdentity;
use crate::log::Seq;
use crate::policy::ReviewLevel;
use crate::process_monitor::ProcessState;
use crate::protocol::status::StatusView;
use crate::protocol::turns::{Landing, QueuedTurn};

mod operations;
mod permissions;
mod phase;
mod starting;
#[cfg(test)]
mod test;
mod usage;

pub use operations::{
    CompactionFailure, CurrentOperations, OpenCompaction, OpenRequest, OpenStage, RunningTool,
};
pub use permissions::{Gate, PendingProposal, RoleView};
pub use phase::{BlockedReason, ConfigRepair, LoopPhase, Stall, StallReason};
pub use starting::{Stage, StartFailure, StartingView};
pub use usage::{
    COLD_STREAK_WARN, Cold, ContextWindow, Cost, KeepaliveSkip, KeepaliveSkipReason, LastCacheMiss,
    UsageView,
};

/// The response header that says how a settle wait ended: `true` when the session settled,
/// `false` when the cap expired and the body is only where it was then (docs/control-api.md).
pub const SETTLED_HEADER: &str = "efference-settled";

/// `GET /state` for a running session: `last_seq`, and one field per state topic, each
/// named by its topic and holding the value its `state` frame carries. A session still
/// starting answers only `last_seq` and the four topics it publishes (`verbs`, `queued`,
/// `starting`, `status`).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StateTopics {
    /// The highest committed `seq`, `null` on an empty log. Not a topic: the log's
    /// cursor rather than control state, so no frame carries it.
    pub last_seq: Option<u64>,
    /// `gate`: the call held for a decision.
    pub gate: Option<Gate>,
    /// `ask`: the question put to the operator.
    pub ask: Option<AskView>,
    /// `processes`: every running tool-spawned process, structure only.
    pub processes: Vec<ProcessState>,
    /// `usage`: token, cache and cost telemetry.
    pub usage: UsageView,
    /// `role`: the effective policy in force.
    pub role: RoleView,
    /// `proposals`: config edits awaiting an orchestrator.
    pub proposals: Vec<PendingProposal>,
    /// `notifications`: ambient notifications not yet shown by a triage.
    pub notifications: Vec<PendingNotification>,
    /// `pace`: the operator's pace dial.
    pub pace: PaceView,
    /// `review_level`: the review dial as set.
    pub review_level: ReviewLevel,
    /// `resolved_review_level`: the review position in force — the dial, except that
    /// `supervisor` with no live supervisor reviews as `spotter_human`.
    pub resolved_review_level: ReviewLevel,
    /// `supervisor`: the session that answers this one's gates under the `supervisor`
    /// review position.
    pub supervisor: Option<String>,
    /// `loop_phase`: where the loop is.
    pub loop_phase: LoopPhase,
    /// `verbs`: what each loop route would answer now.
    pub verbs: Verbs,
    /// `queued`: turns submitted and not yet taken, in submission order.
    pub queued: Vec<QueuedTurn>,
    /// `stall`: a trajectory that stopped short.
    pub stall: Option<Stall>,
    /// `upstream_failure`: the last non-success upstream response, until a 2xx.
    pub upstream_failure: Option<UpstreamFailure>,
    /// `triage_upstream_failure`: the same, for a separate triage client.
    pub triage_upstream_failure: Option<UpstreamFailure>,
    /// `upstream_retry`: a retry in flight.
    pub upstream_retry: Option<UpstreamRetry>,
    /// `cold`: the prompt cache is believed to have expired.
    pub cold: Option<Cold>,
    /// `compaction_failure`: how the last compaction failed.
    pub compaction_failure: Option<CompactionFailure>,
    /// `config_repair`: the config file a human has to fix.
    pub config_repair: Option<ConfigRepair>,
    /// `session_identity`: what the session is.
    pub session_identity: Option<SessionIdentity>,
    /// `current_operations`: what is in flight beside the loop.
    pub current_operations: Option<CurrentOperations>,
    /// `starting`: always `null` once a session has started.
    pub starting: Option<StartingView>,
    /// `status`: where the session stands, as one view.
    pub status: StatusView,
    /// `newer_version`: a release newer than the host.
    pub newer_version: Option<NewerVersion>,
}

/// The loop resource (`GET /loop`, `PATCH /loop`, docs/control-api.md): the operator's
/// dials and where the loop is, each the value — and the name — of its state topic.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct LoopResource {
    pub pace: PaceView,
    pub review_level: ReviewLevel,
    pub loop_phase: LoopPhase,
}

/// The `ask` topic: the question an `ask` has put to the operator
/// (docs/process-monitor.md), as `GET /state` and a status body say it. `null` when none
/// is pending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AskView {
    pub call_id: String,
    pub question: String,
    /// This question's place among the asks of one completion, which are put one at a
    /// time.
    pub n: usize,
    pub of: usize,
    /// The turns withdrawn from the queue when this ask started, in the shape a recall
    /// returns, for the submitting client to put back in its composer.
    pub recalled: Vec<QueuedTurn>,
}

/// The `pace` topic: the operator's between-turns dial (docs/control-channel.md) — loud to
/// the operator's clients, never logged or projected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PaceView {
    /// The delay between steps, in milliseconds.
    pub speed_ms: u64,
    /// The hold: the loop waits at its next juncture until the operator releases it.
    pub held: bool,
}

/// The `verbs` topic: what each loop route would answer now (docs/control-api.md), read
/// from the check the route itself runs. A client renders a refused verb disabled with
/// its message; a request that arrives after the topic moved is checked again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Verbs {
    /// `POST /turns`: always accepted while the session runs, with where the turn would
    /// land.
    pub turn: VerbAnswer,
    /// `POST /loop/continue`.
    #[serde(rename = "continue")]
    pub continue_verb: VerbAnswer,
    /// `POST /loop/compact`.
    pub compact: VerbAnswer,
}

/// One loop route's answer: `accepted`, and either what an accepted request would do or
/// the message a refusal carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct VerbAnswer {
    pub accepted: bool,
    /// An accepted turn's delivery: at once, or queued until the loop's next juncture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Landing>,
    /// An accepted compaction's body: `true` where only a request saying `warm: true`
    /// compacts, because the cache is believed live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm: Option<bool>,
    /// A refusal's message: the one the route's `409` carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl VerbAnswer {
    /// The route would take the request.
    pub fn accepted() -> Self {
        Self {
            accepted: true,
            delivery: None,
            warm: None,
            message: None,
        }
    }

    /// A turn would be taken, landing as `delivery` says.
    pub fn delivered(delivery: Landing) -> Self {
        Self {
            delivery: Some(delivery),
            ..Self::accepted()
        }
    }

    /// A compaction would be taken; `warm` says whether only a `warm: true` body is.
    pub fn compacts(warm: bool) -> Self {
        Self {
            warm: Some(warm),
            ..Self::accepted()
        }
    }

    /// The route would refuse the request with `message`.
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            accepted: false,
            message: Some(message.into()),
            ..Self::accepted()
        }
    }

    /// The message a refusal carries, `None` when the route would take the request.
    pub fn refusal(&self) -> Option<&str> {
        (!self.accepted).then(|| self.message.as_deref().unwrap_or_default())
    }
}

/// One pending ambient notification, as the `notifications` topic lists it (docs/inbox.md):
/// an arrived notification not yet shown by a triage, or a slot notification still queued.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PendingNotification {
    /// The arrival entry's `seq`, the handle a triage's verdict addresses. Absent for a
    /// slot notification, which has no entry until it is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
    pub source: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Must-wake (docs/inbox.md): the loop pins this at its first show whatever the
    /// triage decides, so an idle agent gets a turn to act on it. Set by the
    /// harness for a delegated child's status, never by a client.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wake: bool,
}

/// The `session_identity` topic: what a session is, for a client that has to name it on
/// screen — its id and title, the model it was configured with, and the directory it runs
/// in (docs/control-api.md).
///
/// All but the title are fixed at boot. They are carried as a state topic rather than a
/// one-shot read so a client learns them from the connect snapshot along with
/// everything else, and so either moving later reaches clients through the
/// mechanism that already delivers whole-topic replacements.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SessionIdentity {
    /// The session id (docs/session-title.md): its directory name under
    /// `.efference/`, the `--name` a client resolves, the address a peer
    /// messages. Fixed at creation.
    pub id: String,
    /// The current title — the last `Title` entry's text (docs/session-title.md),
    /// `None` before the first is written. The one identity field that moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The native model id boot resolved — the same string the system prompt's
    /// identity line states (docs/context.md).
    pub model: String,
    /// The session's working directory, as the daemon knows it: what the tool
    /// layer was built with, rendered as a path string.
    pub cwd: String,
    /// The host connection and native model behind each role, with whether
    /// triage shares the main client. `None` only for a session built without
    /// boot (tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference: Option<InferenceIdentity>,
    /// The session this one handed its work to with `hand_off` (docs/tools.md),
    /// once it has. An attached client follows it there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
}

/// The `newer_version` topic: the newest release the host's update check found, while it
/// is newer than the host (docs/update-check.md), and the command that installs it. Host
/// state, the same on every session of a host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct NewerVersion {
    /// The version as the release names it: `0.2.0`.
    pub version: String,
    /// The release's tag: `v0.2.0`.
    pub tag: String,
    /// Where the release's files are published.
    pub url: String,
    /// The command that installs it.
    pub update_command: String,
}
