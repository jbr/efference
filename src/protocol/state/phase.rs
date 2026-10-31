//! Where the loop is, and what stopped it (docs/control-api.md): the `loop_phase`,
//! `stall`, and `config_repair` topics. The loop moves the phase and records the stall
//! and the repair (`daemon/phase.rs`, `daemon/stall.rs`).

use serde::{Deserialize, Serialize};

/// Where the loop is now. Exclusive: what is in flight beside it — open requests,
/// running tools, a compaction — is the `current_operations` topic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LoopPhase {
    /// Waiting for a turn, a continue, or an inbox wake.
    #[default]
    Idle,
    /// A model request is in flight: a turn's, or a triage's (the idle triage included).
    Requesting,
    /// Tool calls are running, the spotter's judgement inside a batch included.
    Executing,
    /// A wait that ends on a timer: a finite pace delay, or the pause between upstream
    /// attempts. `until` is absolute Unix millis.
    Delayed { until: u64 },
    /// Waiting on a decision from outside the loop at an approval gate. An autonomous
    /// decline is decided inside the loop and never enters it.
    Gated,
    /// Waiting on the operator's answer to an `ask`.
    Asking,
    /// Stopped at the cold check, waiting for a person before an uncached read.
    ColdParked,
    /// Stopped until someone outside acts, for a reason that is not a gate, an ask,
    /// or a cold park.
    Blocked { reason: BlockedReason },
}

/// Why the loop is [`LoopPhase::Blocked`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    /// A triage could not reach its model; the results are held (docs/triage.md).
    TriageUnavailable,
    /// A triage never produced a usable verdict; the results are held (docs/triage.md).
    TriageDefect,
    /// The config file does not parse and could not be restored (docs/permissions.md).
    ConfigRepair,
    /// The operator's pace dial is held (docs/control-channel.md).
    PaceHeld,
}

impl LoopPhase {
    /// Whether `settle` returns at this phase: the loop is waiting on someone outside
    /// it, or on nothing (docs/control-api.md). Idle also needs an empty queue, which the
    /// host's settle check reads beside the phase.
    pub fn settles(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Gated | Self::Asking | Self::ColdParked | Self::Blocked { .. }
        )
    }
}

/// Why a trajectory stopped short of finishing (docs/transport.md).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StallReason {
    /// The stream-abort budget was spent: every attempt died before finalizing.
    RetriesExhausted,
    /// The request itself failed after the transport's own retries.
    RequestFailed,
    /// The upstream named a failure no retry can change — an exhausted plan, a
    /// rejected request, a bad credential — so the loop stalled on the first
    /// attempt rather than spend the schedule (docs/transport.md).
    UpstreamRefused,
    /// The triage could not reach its model — the schedule ran out, or the
    /// upstream refused — so the loop holds the batch's results, unshown, until a
    /// retry succeeds or the operator interrupts (docs/triage.md).
    TriageUnavailable,
    /// The triage model answered every fork and never usably: a harness defect,
    /// logged as one. The loop holds the results the same way
    /// (docs/triage.md).
    TriageDefect,
}

/// A trajectory that ended without finishing (docs/transport.md).
///
/// The loop returns to idle on both failure paths above, which leaves the
/// daemon's other signals — the idle phase, no gate, an empty inbox once a
/// triage has dismissed the notification — identical to a run that
/// finished its work. This topic is the difference a supervising client reads:
/// set when the loop gives up, cleared when a trajectory next starts, so it
/// names the *current* condition rather than a past one (the same clear-on-
/// recovery rule the transport diagnostics follow).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Stall {
    pub reason: StallReason,
    /// How many attempts were spent, where the failure path counts them;
    /// `null` where it does not.
    pub attempts: Option<u32>,
    /// Unix millis at which the loop gave up — absolute, like every other
    /// instant on the control channel, so nothing has to re-emit a ticking
    /// field (docs/control-api.md).
    pub at: u64,
    /// Leads with what happened and what recovers it — the same register the
    /// retry topic's `message` uses.
    pub message: String,
    /// The upstream's own words for the failure that ended the trajectory.
    pub detail: String,
}

/// The config file on disk does not parse **and** could not be restored to the
/// last content that did (`config::Refreshed::RevertFailed`) — the one state the
/// adopted-or-rejected invariant cannot reach (docs/permissions.md).
///
/// It is terminal-until-repaired rather than a notice: the effective policy in
/// memory is still the last good config, but the file no longer shows it, and the
/// file is the perimeter's readable form. Running on indefinitely would mean a
/// session acting under permissions nobody can inspect. Making it terminal
/// introduces no blocked-with-nobody-home state, because a config write is the one
/// operation that is never autonomous — it gates for the orchestrator even under
/// `--autonomous`, so a file in this condition always has an owner.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConfigRepair {
    /// The file a human has to fix.
    pub path: String,
    /// Unix millis at which the daemon parked, absolute like every other instant
    /// on the control channel (docs/control-api.md).
    pub at: u64,
    /// Leads with the condition and what clears it — the same register the
    /// `stall` topic's `message` uses.
    pub message: String,
    /// Why the file did not parse.
    pub parse_error: String,
    /// Why the restore of the last good content also failed.
    pub restore_error: String,
}
