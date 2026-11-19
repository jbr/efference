//! The `agents__<verb>` tool surface as the model and the loop see it: the namespace, the
//! wire names the loop treats specially, the `decide` argument, the action a decision
//! binds, and a session's place in the supervision relation, which the spotter is told
//! (docs/tools.md, docs/permissions.md).
//!
//! The verbs themselves are `control_tools`. This is the part the rest of the crate reads
//! without importing them: `tools` mounts the namespace, `config` reserves it, and the
//! loop recognizes the calls whose permission is settled differently from any other's.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::record::log::ToolCall;

/// The wire name of the in-session `decide`: the one call the loop binds to its
/// target's held action before running the permission ladder, when it allows
/// (docs/permissions.md).
pub const DECIDE_WIRE_NAME: &str = "agents__decide";

/// `hand_off`'s wire name in a session's toolbox: the call that gives the
/// session's place to a new one, which is always put to the operator
/// (docs/permissions.md).
pub const HAND_OFF_WIRE_NAME: &str = "agents__hand_off";

/// `archive`'s wire name in a session's toolbox. A call to it naming the caller's own
/// predecessor is the hand-off's completion and clears on the fast path
/// (docs/permissions.md).
pub const ARCHIVE_WIRE_NAME: &str = "agents__archive";

/// A gate decision (docs/permissions.md): `allow` runs the held call; `deny` refuses it;
/// `escalate` leaves it held and passes it to whoever decides next.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
    Escalate,
}

/// The decision an `agents__decide` call's input names, if it names one.
pub(crate) fn decision_of(input: &Value) -> Option<Decision> {
    serde_json::from_value(input.get("decision")?.clone()).ok()
}

/// The leaf action an allowing `agents__decide` call authorizes (docs/permissions.md).
///
/// `agents__decide` carries only a target and a handle, so the parent's ladder would
/// otherwise judge the decision rather than the move it authorizes. Before
/// classification the host binds the target's held call to the decision: the
/// leaf tool, its input as the child wrote it, its intent and cwd, and the chain
/// of sessions from the directly-targeted child down to the session holding the
/// leaf. The parent's fast-path, spotter, and human gate all read this instead
/// of the handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct BoundAction {
    /// The leaf tool's wire name.
    pub name: String,
    /// The leaf call's input, as the child wrote it (its `intent` still inside,
    /// docs/tools.md).
    pub input: Value,
    /// The leaf call's one-line intent, lifted for the gate view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The leaf session's working directory — what its relative paths resolve
    /// against.
    pub cwd: PathBuf,
    /// The leaf session's project directory, which its grants resolve against;
    /// with `cwd`, the directories `policy::classify` runs with.
    pub project: PathBuf,
    /// The session the parent's call directly names — the top of `chain`.
    pub session: String,
    /// The session ids from that target down to the session holding the leaf,
    /// inclusive. One entry for a direct decision; longer when the held call is
    /// itself an allowing decision. Only participating deciders appear.
    pub chain: Vec<String>,
}

impl BoundAction {
    /// The leaf session's project and working directories, for classification.
    pub fn directories(&self) -> crate::config::policy::Directories {
        crate::config::policy::Directories::new(self.project.clone(), self.cwd.clone())
    }
}

/// Whether `call` is an `agents__decide` that allows: the one decision bound to the
/// action it authorizes (docs/permissions.md).
pub(crate) fn allows(call: &ToolCall) -> bool {
    call.name == DECIDE_WIRE_NAME && decision_of(&call.input) == Some(Decision::Allow)
}

/// Whether `call` is an `agents__decide` that denies or escalates. Neither authorizes
/// anything, so both clear on the fast path (docs/permissions.md).
pub(crate) fn authorizes_nothing(call: &ToolCall) -> bool {
    call.name == DECIDE_WIRE_NAME
        && matches!(
            decision_of(&call.input),
            Some(Decision::Deny | Decision::Escalate)
        )
}

/// The session an `agents__archive` call names, when `call` is one that names a session.
/// The two archives that clear on the fast path, a successor's of its predecessor and a
/// supervisor's of a session it supervises, are read by their target (docs/permissions.md).
pub(crate) fn archive_target(call: &ToolCall) -> Option<&str> {
    (call.name == ARCHIVE_WIRE_NAME)
        .then(|| call.input.get("agent").and_then(Value::as_str))
        .flatten()
}

/// A session's place in the supervision relation, as the host records it: the session
/// that supervises it and the sessions it supervises (docs/permissions.md). The prose
/// spotter reads it as facts about the session whose move it judges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Supervision {
    /// The session the host's supervisor pointer names for this one, while that session
    /// is hosted (`Session::supervisor`). `None` for a session with no supervisor.
    pub supervisor: Option<String>,
    /// The hosted sessions whose supervisor pointer names this one, by id.
    pub supervisees: Vec<Supervisee>,
}

/// A session another supervises, as [`Supervision::supervisees`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Supervisee {
    /// The supervised session's id.
    pub id: String,
    /// Whether the supervising session created it (its `created_by`). False for a
    /// session the operator gave it to supervise.
    pub created: bool,
    /// Its status now, in the session listing's words.
    pub status: crate::protocol::sessions::ListedStatus,
}

#[cfg(test)]
mod test;
