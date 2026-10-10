//! The durable half of an upstream request's lifecycle (docs/log-and-projection.md).
//!
//! A request is one HTTP exchange with the provider that streams a completion: a
//! turn, a peek, a spotter assessment, a title ask, a keepalive ping. Each leaves a
//! few [`RequestStage`] entries as it passes the stages a reader waiting on it can
//! tell apart. They answer "what was it doing, and since when" after the fact and
//! feed the `current_operations` state topic live (docs/control-api.md). Streaming
//! sub-stages (thinking, text, a tool call) are not recorded; the ephemeral `output`
//! class distinguishes them.
//!
//! None of this reaches the wire: [`Payload::RequestStage`] projects to nothing, like
//! [`Payload::ToolTelemetry`], and the entry's own `at` is the stage's time, so no
//! clock reading is stored here.
//!
//! [`Payload::RequestStage`]: crate::log::Payload::RequestStage
//! [`Payload::ToolTelemetry`]: crate::log::Payload::ToolTelemetry

use serde::{Deserialize, Serialize};

/// Which request shape spent the tokens (docs/log-and-projection.md) — the tag on each
/// persisted usage line, so per-shape spend is a query over `usage.jsonl` rather than a
/// reconstruction from the canonical log.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RequestOrigin {
    /// An agent-loop completion (docs/README.md).
    Turn,
    /// A fork-peek round-trip riding tool results (docs/redaction-fork.md), retries
    /// included.
    Peek,
    /// A standalone idle-triage peek (docs/supervisor.md).
    Triage,
    /// A title peek (docs/session-title.md) — the triage model asked, off the
    /// cached prefix, what the session is for.
    Title,
    /// A spotter assessment (docs/permissions.md) — its own prefix, its own lineage.
    Spot,
    /// A cache keepalive ping (docs/log-and-projection.md).
    Keepalive,
    /// A triage-lineage primer (backlog/separate-triage-cold-prefix.md): the turn's own
    /// input replayed on the own peek's client so the result-triage peek resumes where
    /// the main request ended. Never main-prefix evidence: it plants a resume point on
    /// the triage lineage, not the loop's.
    TriagePrimer,
    /// A keepalive ping on the triage lineage: the primer's own envelope re-sent over the
    /// primed turns (docs/transport.md). It refreshes the triage lineage's resume point
    /// and never main-prefix evidence.
    TriageKeepalive,
    /// The narrator writing a fold's account (docs/summary-head.md): its own prompt,
    /// so it does not touch the main prefix.
    Narrate,
    /// A retried attempt the loop recorded and re-sent — an unfinalized stream
    /// (all-zero usage) or one that finalized unparseable (real usage;
    /// docs/transport.md). Persisted as its own origin so it is not read as a
    /// real main-loop request: only its non-zero usage is priced
    /// (`Session::spawn_cost_fetch`) and it never counts as prefix evidence
    /// (`ColdStreak::observe`).
    AbortedAttempt,
    /// An origin this build does not recognize — a forward-compat catch-all so a
    /// `usage.jsonl` line written by a newer build still parses here (and, per
    /// serde's `#[serde(other)]` contract, round-trips as this variant's own name
    /// rather than the original one if ever re-serialized).
    #[serde(other)]
    Unknown,
}

impl RequestOrigin {
    /// Whether every request of this origin read the main loop's own cached prefix, and
    /// so refreshed it. Turns and keepalive pings do. A peek may run on the agent's own
    /// client (it reads the main prefix) or on a separate triage client with a prefix of
    /// its own; the origin does not say which, so the peek's line carries
    /// [`UsageLine::main_prefix`](crate::daemon::UsageLine) when it ran on the agent's client. Title and triage
    /// requests are left out, which can only make the cache look colder than it is.
    pub fn touches_main_prefix(self) -> bool {
        matches!(self, Self::Turn | Self::Keepalive)
    }

    /// Whether every request of this origin read the triage lineage's own cached prefix
    /// (docs/transport.md): the primer and the triage keepalive do, and a result-triage
    /// peek does only when it ran on its own client, which the origin does not say, so
    /// `Session::record_peek` decides for it.
    pub fn touches_triage_prefix(self) -> bool {
        matches!(self, Self::TriagePrimer | Self::TriageKeepalive)
    }
}

/// One stage of one upstream request (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestStage {
    /// The request's id, unique within the session: minted by the host when the
    /// request starts, because the provider's generation id arrives late or never.
    /// Entries of one request share it; concurrent requests interleave by it.
    pub request: u64,
    pub origin: RequestOrigin,
    pub stage: RequestStageKind,
    /// On `Ended`: the provider's own word for why the output ended (`end_turn`,
    /// `tool_calls`, `incomplete:max_output_tokens`, …), as the dialect spelled it
    /// (`api::Finish`). Absent on other stages, on a stream that named none, and on
    /// entries written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
}

impl RequestStage {
    /// A stage with no finish word: every stage but a named `Ended`.
    pub fn new(request: u64, origin: RequestOrigin, stage: RequestStageKind) -> Self {
        Self {
            request,
            origin,
            stage,
            finish: None,
        }
    }
}

/// Where a request is, in the order a request passes them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStageKind {
    /// The request was handed to the transport.
    Sent,
    /// An attempt got a non-success status; the transport may re-send it. `attempt`
    /// is 1-based within the request. The request stays open until the next stage.
    AttemptFailed { attempt: u32, status: u16 },
    /// The stream opened and its first event arrived.
    FirstByte,
    /// The stream finalized.
    Ended,
    /// The request ended without a finalized stream: an open failure, a stream that
    /// died or ran past its ceiling, or `interrupted` (the operator's interrupt, or
    /// the request's future dropped).
    Failed { reason: String },
}

impl RequestStageKind {
    /// Whether the request is over after this stage.
    pub fn closes(&self) -> bool {
        matches!(self, Self::Ended | Self::Failed { .. })
    }
}
