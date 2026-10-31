//! The session's permissions as its clients read them (docs/permissions.md): the call
//! held at the gate, the config edits awaiting an orchestrator, and the effective policy
//! in force — the `gate`, `proposals`, and `role` topics. The review dial and the
//! supervisor are the other permission topics; the dial is a
//! [`ReviewLevel`](crate::policy::ReviewLevel).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::config::{CommandStance, Perms, ToolSettings};
use crate::log::{ConfigChange, Seq};
use crate::policy::GateGrant;
use crate::protocol::agents::BoundAction;
use crate::protocol::status::Escalated;

/// The `gate` topic: the call held for a decision, as `GET /state` surfaces it — the
/// ephemeral control state the log does not record, since it stores only the result.
/// `null` when no gate is open.
#[derive(Clone, Debug, Serialize)]
pub struct Gate {
    pub call_id: String,
    /// The held tool's wire name.
    pub name: String,
    /// The `tool_use` input, verbatim.
    pub input: Value,
    /// The spotter's one-line reason for routing this call to the human, if it gated via the
    /// spotter (docs/permissions.md) — read from the `Spot` audit entry the spotter logs *before*
    /// the gate. Lets the client show *why* a call is held without a new side channel. `None` when
    /// no spotter verdict preceded the gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The leaf action this approval authorizes, when the held call is a bound
    /// `agents__decide` (docs/permissions.md). `None` for an ordinary gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acting_on: Option<BoundAction>,
    /// The one grant this gate offers, decided at the rung that held the call
    /// (docs/permissions.md). `None` when no single change is confidently the one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<GateGrant>,
    /// Each time the call was passed up undecided, in order (`decide` with `escalate`,
    /// docs/permissions.md); the last names whom it is routed to now.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<Escalated>,
}

/// One async config edit awaiting an orchestrator decision
/// (docs/permissions.md). Filed by a `Proposed` [`ConfigEdit`](crate::log::ConfigEdit) append (the
/// session's `propose_scope`, the spotter's prefix-promotion); the orchestrator-above commits or
/// cancels it, which drops it from the queue. Unlike a
/// [`PendingNotification`](super::PendingNotification), the proposal
/// **already has a durable `seq`** (the `Proposed` entry's), so that *is* its id — no
/// queue-local counter.
#[derive(Clone, Debug, Serialize)]
pub struct PendingProposal {
    /// The `seq` of the `Proposed` log entry — the proposal's id, which the orchestrator's
    /// commit/cancel references.
    pub seq: Seq,
    /// The change being proposed; the committing entry copies it self-contained so the fold stays a
    /// pure per-entry filter (docs/permissions.md).
    pub change: ConfigChange,
}

/// The `role` topic: the **effective** policy in force — the base config with every
/// committed config edit folded in, so the live perimeter rather than the boot config
/// (docs/permissions.md). It carries the policy's contents, not counts: an orchestrator
/// inspecting a subordinate needs the commands, paths and scope actually in force.
#[derive(Clone, Debug, Serialize)]
pub struct RoleView {
    /// The base config's path, or `null` for the built-in default.
    pub source: Option<String>,
    /// The scope shared with the session; it can drift from the frozen preamble through
    /// a live scope edit.
    pub scope: Option<String>,
    /// What the spotter watches for, which is on no other wire.
    pub watch_for: Vec<String>,
    /// Each `[commands]` key with the stance it carries: an entry can carve `watch`,
    /// `gate` or `deny` out of a broader grant, so a reader that saw only the keys would
    /// read a carve-out as a grant.
    pub commands: BTreeMap<String, CommandStance>,
    /// The `[tools]` table keyed as the file spells each entry (`efference`,
    /// `"agents.message"`), carrying only the fields the entry sets: a `granted = false`
    /// carve-out under a granted namespace reads as one.
    pub tools: BTreeMap<String, ToolSettings>,
    /// The `[paths]` grants.
    pub paths: BTreeMap<String, Perms>,
}
