//! The settle predicate: whether the loop is at a decision point, decided from one
//! snapshot of the phase and what waits in front of the loop (docs/control-channel.md).

use super::*;

/// The loop phase and what waits in front of the loop, as they stood at one instant
/// ([`Session::phase_and_queue`]): what `settle`, a turn's delivery, and a continue's
/// verdict each decide from (docs/control-channel.md).
// Separate observations read at one instant, each consumer reading a different subset;
// the flags are not alternatives of one state.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct PhaseAndQueue {
    /// The phase, as [`Session::loop_phase`] reports it.
    pub(super) phase: LoopPhase,
    /// The phase reads `delayed` for a pause between upstream attempts, not a pace delay.
    pub(super) retry_pause: bool,
    /// A queued turn the loop will run: one the operator's hold has not held back.
    pub(crate) turn_to_run: bool,
    /// The queue holds a turn, a held-back one included.
    pub(super) queued: bool,
    /// A continue is waiting for the loop.
    pub(super) continue_pending: bool,
}

impl Session {
    /// Pinged whenever the session may have reached a settled state (a gate appeared, the
    /// loop went idle); the `/settle` long-poll and the status watcher wake on it.
    pub(crate) fn settle_changed(&self) -> &Notify {
        &self.settle_changed
    }

    /// Whether the daemon is at a **decision point** a client can act on: the loop phase is one
    /// that waits on someone outside the loop — gated, asking, cold-parked, blocked — or idle
    /// with nothing queued (docs/control-api.md). A request, a running batch, and a timed delay
    /// are *not* settled. The predicate behind `/settle` and the `?wait` flag, decided from one
    /// [`PhaseAndQueue`].
    pub(crate) fn is_settled(&self) -> bool {
        let now = self.phase_and_queue();
        match now.phase {
            // An accepted `POST /loop/continue` (docs/control-api.md) is a turn
            // about to run, exactly as a queued submission is.
            LoopPhase::Idle => !now.turn_to_run && !now.continue_pending,
            phase => phase.settles(),
        }
    }

    /// Read the phase, the queue, the turns the hold holds back, and the continue flag as
    /// they stood at one instant (docs/control-channel.md). The pace and queue locks are held
    /// across every read, and all but the phase are written only under one of them, so none
    /// moves while the phase is read. [`claim_turn`] and [`take_continue`] move the phase
    /// with the queue lock held too, so this sees a turn or a continue still waiting or
    /// already taken, never neither. Read under separate locks, the two can come from
    /// different instants: an idle phase from before a claim with the queue the claim
    /// emptied, or a queue read mid-turn with the idle that turn ends in after a submit —
    /// "idle, nothing queued", a state that never held.
    ///
    /// The locks are taken pace, queue, phase: the order [`claim_turn`] and
    /// [`no_turn_queued_to_run`](Self::no_turn_queued_to_run) take them in.
    ///
    /// [`claim_turn`]: Session::claim_turn
    /// [`take_continue`]: Session::take_continue
    pub(crate) fn phase_and_queue(&self) -> PhaseAndQueue {
        let pace = self.pace.lock().unwrap();
        let queue = self.queue.lock().unwrap();
        let (phase, retry_pause) = self.phase_and_retry_pause();
        // Where a test lands a writer between the reads (`between_reads`).
        #[cfg(test)]
        {
            let interleaved = self.between_reads.lock().unwrap().take();
            if let Some(interleaved) = interleaved {
                interleaved(self);
            }
        }
        PhaseAndQueue {
            phase,
            retry_pause,
            turn_to_run: pace.turn_to_run_in(&queue),
            queued: !queue.is_empty(),
            continue_pending: self.continue_pending(),
        }
    }

    /// Block until [`is_settled`]. Uses `Notified::enable` to register the waiter **before** the
    /// condition check: `settle_changed` is a `notify_waiters` (no stored permit), so a
    /// `going_idle`/`register_pending` that fires between the check and the await would otherwise be
    /// lost. With the waiter pre-registered, the wake is delivered to the pending future.
    ///
    /// [`is_settled`]: Session::is_settled
    pub(crate) async fn await_settled(&self) {
        loop {
            let notified = self.settle_changed.notified();
            tokio::pin!(notified);
            // Register on the waiter queue now, so a notify racing the check below isn't missed.
            notified.as_mut().enable();
            if self.is_settled() {
                return;
            }
            notified.await;
        }
    }
}
