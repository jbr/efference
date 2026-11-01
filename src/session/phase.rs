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
        self.phase_and_retry_pause().0
    }

    /// [`loop_phase`](Self::loop_phase), with whether it reads `delayed` for a pause between
    /// upstream attempts rather than a pace delay: both from one read of the retry cell's
    /// mark, which can clear between two.
    pub(super) fn phase_and_retry_pause(&self) -> (LoopPhase, bool) {
        let phase = *self.phase.lock().unwrap();
        match (phase, self.upstream_retry.delaying()) {
            (LoopPhase::Requesting, Some(until)) => (LoopPhase::Delayed { until }, true),
            _ => (phase, false),
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
}
