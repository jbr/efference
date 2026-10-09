//! The durable record of the cold check (docs/transport.md, docs/control-api.md): when
//! the loop parked because the cache is believed expired, when a consent arrived and
//! from which gesture, and when the park ended.
//!
//! None of it reaches the wire: [`Payload::Cold`] projects to nothing, like
//! [`Payload::Push`], and the entry's own `at` is the time of each step.
//!
//! [`Payload::Cold`]: crate::log::Payload::Cold
//! [`Payload::Push`]: crate::log::Payload::Push

use serde::{Deserialize, Serialize};

use super::Timestamp;

/// Why the loop stopped at its cold check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdReason {
    /// The cache is believed expired: continuing pays the context uncached.
    Expired,
}

/// The gesture that carried `accept_cold_read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentSource {
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
    /// A consent was taken and the request went out.
    Consent,
    /// The belief ended without one (a fold, or another request warmed the cache).
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
    /// A consent was recorded while the session was believed cold.
    Consented { source: ConsentSource },
    /// The loop left the cold check.
    Unparked { cause: UnparkCause },
}
