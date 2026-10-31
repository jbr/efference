//! Where a session stands, as one view (docs/control-api.md, docs/tools.md): the
//! `status` topic, and what a subscription notification and a status read render.
//!
//! The host builds the view from the loop phase and the state each condition needs
//! (`daemon/status/view.rs`) and prints it with one renderer (`daemon/status/render.rs`).
//! Instants are absolute Unix millis, so the topic changes only at a transition; the
//! renderer subtracts them from its own clock.

use serde::{Deserialize, Serialize};

use crate::log::Seq;
use crate::protocol::state::{Cost, Stage};

/// The most of a closing message the view carries. Past it the message is cut on a char
/// boundary and the view says how much was left out.
pub const CLOSING_CAP: usize = 4096;

/// One session's status, as every surface renders it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusView {
    pub id: String,
    pub condition: Condition,
    pub footer: Footer,
}

/// The condition, and what each one carries for its reader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// Being prepared: the `starting` topic's stage and authorization.
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

/// A queued message a question returned undelivered: who sent it and what it said.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Returned {
    /// The turn's `source`: `None` for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub text: String,
}

/// A held call, as its decider reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldCall {
    /// The provider's id for the call, which the approval route takes; `decide` maps
    /// the handle to it.
    pub call_id: String,
    /// The tool's display name (`fs.list`).
    pub tool: String,
    /// What it does: a command, or the arguments (`transcript::call_summary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The transcript handle, `seq` or `seq.n`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Why it was held, in plain words (docs/permissions.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_because: Option<String>,
    pub reviewer: Reviewer,
    /// Each time the call was passed up undecided, in order; the last names whom it is
    /// routed to now (docs/permissions.md).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<Escalated>,
}

/// Who decides a held call (docs/permissions.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reviewer {
    Operator,
    /// The supervising session the gate is routed to: the holder's supervisor, or one
    /// further up the supervisor chain that an escalation passed it to.
    Supervisor {
        id: String,
    },
}

/// One escalation of a held call: who passed it up, to whom, and the note it carries
/// for whoever decides next (docs/permissions.md). The gate topic and the status view
/// both carry these (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalated {
    /// Who escalated, spelled as a turn's `source` is: `session[<id>]`, `mcp[<name>]`;
    /// `null` for the operator's own command line.
    pub source: Option<String>,
    /// The session it went to; `null` for the operator.
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A turn's closing message, cut at [`CLOSING_CAP`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closing {
    /// The entry it was read from.
    pub seq: Seq,
    pub text: String,
    /// The whole message's size and line count, so a cut can say how much it left out.
    pub bytes: usize,
    pub lines: usize,
}

/// Why a loop is parked at its cold check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Park {
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
pub enum Block {
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

/// One thing a working loop is doing: an open operation, a retry, or the pace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Activity {
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
/// leave it out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Footer {
    /// The main model's native id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The last turn's prompt, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    /// The model's context window, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u64>,
    /// The session's cost, where the provider prices requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<Background>,
    /// Each queued turn's `source`, in submission order: `None` for the operator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<Option<String>>,
    /// The operator holds the session: nothing queued runs until the hold is released
    /// (docs/control-channel.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

/// A background process the session holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background {
    pub id: u64,
    pub command: String,
    pub since: u64,
}
