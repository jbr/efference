//! Where the loop is (docs/control-api.md): one exclusive [`LoopPhase`], set from the
//! loop's control flow and published as the `loop_phase` topic.
//!
//! The loop sets the phase at each step it takes — a request, a batch of calls — and
//! each wait it enters names itself for as long as it lasts ([`Session::wait_phase`]).
//! A decision that ends a wait from outside the loop (an approval, an answer, a
//! release of the hold, a person acting at the cold check) moves the phase at the
//! moment it is taken ([`Session::end_wait`]), so a `settle` racing the loop's wake
//! does not read the wait that just ended. The one phase not stored here is the
//! pause between upstream attempts: the retry cell marks it (`api/diagnostics.rs`),
//! because the triage's attempts run where no `Session` is held.

use super::*;

/// Where the loop is now. Exclusive: what is in flight beside it — open requests,
/// running tools, a compaction — is the `current_operations` topic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LoopPhase {
    /// Waiting for a turn, a continue, or an inbox wake.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
    /// it, or on nothing (docs/control-api.md). Idle also needs an empty queue, which
    /// [`Session::is_settled`] checks.
    pub fn settles(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Gated | Self::Asking | Self::ColdParked | Self::Blocked { .. }
        )
    }
}

/// A wait the loop is in, named as the phase while it lasts. Dropping it ends the
/// wait into the phase the loop moves to next, unless a decision from outside already
/// ended it.
pub(crate) struct PhaseWait<'a> {
    session: &'a Session,
    wait: LoopPhase,
    then: LoopPhase,
}

impl Drop for PhaseWait<'_> {
    fn drop(&mut self) {
        self.session.end_wait(|phase| phase == self.wait, self.then);
    }
}

impl Session {
    /// The phase now: the stored phase, read as [`LoopPhase::Delayed`] while a request
    /// waits between upstream attempts.
    pub fn loop_phase(&self) -> LoopPhase {
        let phase = *self.phase.lock().unwrap();
        match (phase, self.upstream_retry.delaying()) {
            (LoopPhase::Requesting, Some(until)) => LoopPhase::Delayed { until },
            _ => phase,
        }
    }

    /// Move the loop to `phase`, returning the one it left. Wakes `settle` waiters and
    /// the topic differ, since either may read it.
    pub(crate) fn set_phase(&self, phase: LoopPhase) -> LoopPhase {
        let previous = std::mem::replace(&mut *self.phase.lock().unwrap(), phase);
        if previous != phase {
            self.settle_changed.notify_waiters();
            self.touch_state();
        }
        previous
    }

    /// Enter the wait `wait`, ending into `then` when the returned guard drops.
    pub(crate) fn wait_phase(&self, wait: LoopPhase, then: LoopPhase) -> PhaseWait<'_> {
        self.set_phase(wait);
        PhaseWait {
            session: self,
            wait,
            then,
        }
    }

    /// End the wait the loop is in, if `is_wait` names it, moving to `then`. Called by
    /// the decision that ends it, and by the loop's own [`PhaseWait`] on its way out;
    /// whichever comes second finds the wait already over and does nothing.
    pub(crate) fn end_wait(&self, is_wait: impl Fn(LoopPhase) -> bool, then: LoopPhase) {
        let mut phase = self.phase.lock().unwrap();
        if !is_wait(*phase) {
            return;
        }
        *phase = then;
        drop(phase);
        self.settle_changed.notify_waiters();
        self.touch_state();
    }

    /// The `loop_phase` topic.
    pub(super) fn loop_phase_json(&self) -> Value {
        serde_json::to_value(self.loop_phase()).unwrap_or(Value::Null)
    }
}
