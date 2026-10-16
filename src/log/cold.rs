//! The durable record of the cold check (docs/transport.md, docs/control-api.md): when
//! the loop parked because the cache is believed expired, when a person's action
//! released it and which one, and when the park ended.
//!
//! None of it reaches the wire: [`Telemetry::Cold`] projects to nothing, like
//! [`Telemetry::Push`], and the entry's own `at` is the time of each step.
//!
//! [`Telemetry::Cold`]: crate::log::Telemetry::Cold
//! [`Telemetry::Push`]: crate::log::Telemetry::Push

use serde::{Deserialize, Serialize};

use super::Timestamp;

/// Why the loop stopped at its cold check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdReason {
    /// The cache is believed expired: continuing pays the context uncached.
    Expired,
}

/// The person action that released a park (or would have: it is recorded whenever a
/// person acts while the session is believed cold).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasedBy {
    /// `POST /turns` (or the `turn` tool) with no `ask` pending.
    Turn,
    /// `PUT /approvals/:call_id`: the decision at an approval gate.
    Approval,
    /// `POST /turns` while an `ask` was pending: the answer.
    Ask,
    /// `POST /loop/continue`.
    Continue,
}

/// Why a park ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnparkCause {
    /// A person acted and the request went out.
    Person,
    /// The belief ended without one (a compaction, or another request warmed the cache).
    Warm,
}

/// One step of the cold check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ColdEvent {
    /// The loop stopped before a request on the main prefix.
    Parked {
        reason: ColdReason,
        /// From when the cache is believed expired.
        since: Timestamp,
        /// What the read would cost uncached, when a turn has reported it.
        context_tokens: Option<u64>,
    },
    /// A person acted while the session was believed cold.
    Released { by: ReleasedBy },
    /// The loop left the cold check.
    Unparked { cause: UnparkCause },
}
