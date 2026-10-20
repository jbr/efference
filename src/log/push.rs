//! The durable record of a push (docs/web-push.md): why one was sent, held, or
//! dropped, and what each push service answered.
//!
//! A **moment** is one firing of a gate, an `ask`, or a settle. Every entry about
//! it carries the same `moment` number (unique within the session), so the log
//! reads as a short story: decided, then released or cancelled, then the
//! per-device delivery. None of it reaches the wire: [`Telemetry::Push`] projects to
//! nothing, like [`Telemetry::RequestStage`], and the entry's own `at` is the
//! time of each step.
//!
//! [`Telemetry::Push`]: crate::log::Telemetry::Push
//! [`Telemetry::RequestStage`]: crate::log::Telemetry::RequestStage

use serde::{Deserialize, Serialize};

/// Which situation fired the moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentKind {
    /// An approval gate opened.
    Gate,
    /// An `ask` was posed.
    Ask,
    /// A turn ended and the loop went idle.
    Settle,
}

/// One step in a moment's push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRecord {
    /// The moment's number, unique within the session.
    pub moment: u64,
    pub kind: MomentKind,
    #[serde(flatten)]
    pub event: PushEvent,
}

/// What happened to the moment's push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PushEvent {
    /// No client was attending, so the push went out at once.
    Sent,
    /// A client was attending, so the push waits `delay_secs` to see whether the
    /// moment resolves or is seen first. `attentive` names who was attending.
    Held {
        delay_secs: u64,
        attentive: Vec<AttentiveClient>,
    },
    /// The wait ran out with the moment still standing; the push went out.
    Released,
    /// The held push was dropped.
    Cancelled { cause: PushCancel },
    /// What the push services answered: one element per stored subscription.
    /// Empty means no device is subscribed.
    Delivery { devices: Vec<DeviceDelivery> },
}

/// A client that was attending when the moment fired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentiveClient {
    pub client: String,
    /// The session it had open, if any.
    pub session: Option<String>,
}

/// Why a held push never went out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum PushCancel {
    /// The gate was approved, redirected, or aborted.
    GateDecided,
    /// The `ask` ended without a reply.
    AskEnded,
    /// A client submitted a turn.
    TurnSubmitted,
    /// The session was archived.
    Archived,
    /// An active client reported viewing the session.
    Seen { client: String },
    /// A newer moment replaced it.
    Superseded,
}

/// One subscription's answer to one push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDelivery {
    /// The endpoint's host (`web.push.apple.com`), which names the browser vendor.
    pub host: String,
    /// A short stable hash of the endpoint, so two profiles on one vendor differ
    /// without the log holding the endpoint's secret path.
    pub device: String,
    #[serde(flatten)]
    pub outcome: DeliveryOutcome,
}

/// What the push service said.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// A 2xx.
    Delivered { status: u16 },
    /// 404 or 410: the subscription no longer exists and was pruned.
    Gone { status: u16 },
    /// Any other status (`status` set) or a failure before an answer.
    Failed { status: Option<u16>, reason: String },
}
