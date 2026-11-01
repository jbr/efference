//! The small bodies the control routes answer with, apart from the surfaces that have a
//! module of their own (docs/control-api.md): a word, a key, a proposal's resolution, a
//! tool call and the entry that holds it, a process's retained output, and the model
//! catalog's list.
//!
//! The host serializes these through `routes::views::answer`, a `serde_json::Value` round
//! trip that sorts the keys of every object it holds, so a type's field order is not what
//! reaches the bytes (`host/test/route_bytes.rs`).

use serde::{Deserialize, Serialize};

use crate::inference::ConnectionInventory;
use crate::inference::catalog::ConnectionCatalog;
use crate::record::log::{EditState, Seq, ToolCall};

/// The word a route answers `status` with when it has acted on a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum StatusWord {
    /// `POST /turns`: the turn is queued or delivered (`TurnAccepted`).
    Accepted,
    /// `PUT /approvals/:call_id`: the decision reached the held call.
    Resolved,
    /// `POST /approvals/:call_id/escalate`: the call stays held, and the operator is asked.
    Escalated,
    /// `POST /loop/interrupt`: the running tool was signalled.
    Interrupted,
    /// `POST /loop/continue`: the loop will take another turn on the wire it has.
    Continuing,
    /// `POST /loop/compact`: the session's model is writing the summary.
    Compacting,
    /// `DELETE /processes/:id`: the process's group was signalled.
    Killed,
    /// `POST /push/subscriptions`: the subscription is recorded.
    Subscribed,
    /// `DELETE /push/subscriptions`: the subscription is not held.
    Unsubscribed,
    /// `POST /pair/claim`: the sign-in waits for the operator's approval.
    Pending,
}

/// An answer whose word is all it has to say: `{"status": <word>}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StatusBody {
    pub status: StatusWord,
}

impl StatusBody {
    pub fn new(status: StatusWord) -> Self {
        Self { status }
    }
}

/// The answer to `POST /notifications`: where the notification arrived in the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct FiledNotification {
    /// The arrival entry's seq. Absent for a notification on a slot, which has no entry
    /// until a triage shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
}

/// The key a browser subscribes to push with, from `GET /push`: the machine's VAPID public
/// key, which is `applicationServerKey` verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PushKey {
    /// Base64url of the uncompressed P-256 point.
    pub key: String,
}

/// The state a proposal reached, from `PATCH /proposals/:seq`: `committed` or `cancelled`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProposalResolution {
    pub state: EditState,
}

/// A tool call and the entry that carries it, from `GET /tool_calls/:call_id`. A call is
/// embedded in an `Assistant` entry rather than being one, so this is its address.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct LocatedToolCall {
    pub seq: Seq,
    pub call: ToolCall,
}

/// One cursor read of a process's retained output, from `GET /processes/:id/output`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProcessOutputRead {
    pub id: u64,
    /// The retained bytes from the caller's cursor to the end, lossily decoded. Empty when
    /// the cursor is already at the end.
    pub output: String,
    /// Pass back as `since` to resume after what this read returned.
    pub cursor: u64,
    /// Bytes between the caller's cursor and the earliest retained byte, dropped at the
    /// retention cap and reported rather than skipped.
    pub lost: u64,
    pub running: bool,
}

/// The host's configured connections, from `GET /inference`, sorted by name. Whether each
/// can be used, and nothing that would let a client reach it: no endpoint, no credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct InventoryList {
    pub connections: Vec<ConnectionInventory>,
}

/// The connections of the model catalog, from `GET /inference/catalog`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CatalogList {
    pub connections: Vec<ConnectionCatalog>,
}

#[cfg(test)]
mod test;
