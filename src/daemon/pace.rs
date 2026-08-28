//! Operator pacing, the turn seam, and settle/interrupt (docs/control-channel.md).

use super::*;

/// The operator's pacing state (docs/control-channel.md): a continuous speed dial
/// plus a pause **overlay**. `effective_sleep = held ? ∞ : speed` — `held` is an overlay, *not* a
/// speed value, so the dial retains `speed` while held and releasing returns to the retained speed,
/// never to zero. Freely adjustable while held and outliving it, so a change made during a pause
/// "recolors" what resume does.
#[derive(Clone, Copy, Default)]
pub(super) struct Pace {
    /// The between-turns delay when not held (`0` = as fast as possible, the default).
    pub(super) speed: Duration,
    /// The pause overlay: while set, the effective sleep is infinite (wait at the juncture until
    /// released or a turn is submitted). Retains `speed` underneath.
    pub(super) held: bool,
}

impl Shared {
    pub(super) fn submit_turn(&self, turn: SubmittedTurn) {
        // Count the turn as queued *before* sending, so `is_settled` reads "busy" the instant
        // the turn is accepted — even before the loop dequeues it (the `/settle` submit race,
        // docs/permissions.md).
        self.queued.fetch_add(1, Ordering::SeqCst);
        // Submit ⇒ resume, always (docs/control-channel.md): drop any hold and wake an
        // in-flight juncture sleep so the message is delivered at the next juncture *immediately*, not
        // after the remaining paced delay. The retained `speed` is untouched (it recolors later
        // junctures); only the `held` overlay drops.
        {
            let mut p = self.pace.lock().unwrap();
            p.held = false;
        }
        self.pace_interrupt.notify_waiters();
        // The receiver lives in the loop's `RemoteFrontend`; a send fails only if
        // the loop has exited, in which case the daemon is shutting down anyway.
        let _ = self.turns_tx.send(turn);
    }

    /// The current pace `(speed, held)` — surfaced on `GET /state` and read by the juncture sleep.
    pub(super) fn pace_snapshot(&self) -> (Duration, bool) {
        let p = self.pace.lock().unwrap();
        (p.speed, p.held)
    }

    /// Apply an operator pace change (docs/control-channel.md — the `PATCH /loop {pace}` verb):
    /// set the `speed` dial and/or the `held` overlay, each optional. **Releasing** a hold (`held`
    /// true → false) wakes an in-flight juncture sleep so resume is immediate; a pure speed change or a
    /// *hold* does not wake it — those take effect at the next juncture, per the design (a hold lands at
    /// the next juncture, letting the in-flight step finish; a speed change outlives the hold and
    /// recolors resume). Returns the resulting `(speed, held)`.
    pub(super) fn set_pace(&self, speed: Option<Duration>, held: Option<bool>) -> (Duration, bool) {
        let mut wake = false;
        let result = {
            let mut p = self.pace.lock().unwrap();
            if let Some(s) = speed {
                p.speed = s;
            }
            if let Some(h) = held {
                if p.held && !h {
                    wake = true; // releasing a hold ends the juncture sleep now
                }
                p.held = h;
            }
            (p.speed, p.held)
        };
        if wake {
            self.pace_interrupt.notify_waiters();
        }
        self.touch_state();
        result
    }

    /// The interruptible between-turns sleep (docs/control-channel.md). Applied at the
    /// juncture — after a step's results land, before the next model call — and **transparent to the
    /// agent**: it touches no log entry, so a held/slowed loop projects byte-identically to a fast
    /// one. Waits `effective_sleep = held ? ∞ : speed`, but returns *immediately* if a turn is
    /// already queued (never pace over pending input) and ends early when the `pace_interrupt` fires
    /// (a release or a submitted turn). A finite sleep that expires with no input just proceeds.
    ///
    /// Lost-wakeup safe: the interrupt future is armed (`enable`) *before* the state is read, so a
    /// release/submit racing the read still wakes the wait (mirrors [`await_settled`]).
    ///
    /// [`await_settled`]: Shared::await_settled
    pub async fn pace_at_seam(&self) {
        let interrupt = self.pace_interrupt.notified();
        tokio::pin!(interrupt);
        interrupt.as_mut().enable(); // register before reading state, so a racing wake isn't lost
        // A submitted turn always wins — deliver it now rather than pacing over it.
        if self.queued.load(Ordering::SeqCst) > 0 {
            return;
        }
        let (speed, held) = self.pace_snapshot();
        if held {
            // Infinite hold: wait until released or a turn is submitted, then proceed.
            interrupt.await;
        } else if !speed.is_zero() {
            // Paced: proceed when the delay elapses or an interrupt (submit/release) ends it early.
            tokio::select! {
                _ = tokio::time::sleep(speed) => {}
                _ = interrupt => {}
            }
        }
        // Not held, no delay → proceed at once.
    }

    /// The loop dequeued a turn: mark running, then consume the queued count — `running` set
    /// *before* the decrement so [`is_settled`] never sees both clear while a turn is in flight.
    ///
    /// [`is_settled`]: Shared::is_settled
    pub(super) fn dequeued_turn(&self) {
        self.running.store(true, Ordering::SeqCst);
        self.queued.fetch_sub(1, Ordering::SeqCst);
    }

    /// A turn was drained mid-trajectory at the injection juncture
    /// (docs/control-channel.md): consume its `queued` count so `/settle` can
    /// still settle once the trajectory ends, but leave `running` alone — the loop is already
    /// running, so unlike [`dequeued_turn`] there's no busy edge to guard. Mirrors only the
    /// queued-count half of the dequeue.
    ///
    /// [`dequeued_turn`]: Shared::dequeued_turn
    pub(super) fn injected_turn_dequeued(&self) {
        self.queued.fetch_sub(1, Ordering::SeqCst);
    }

    /// The loop is parking (about to await the next user turn): mark not-running and wake any
    /// `/settle` waiter. If a turn is already queued, `recv` resolves at once and [`dequeued_turn`]
    /// re-sets running, so a transient `running == false` with `queued > 0` still reads as busy.
    ///
    /// [`dequeued_turn`]: Shared::dequeued_turn
    pub(super) fn parking(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.settle_changed.notify_waiters();
    }

    /// Whether the daemon is at a **decision point** a client can act on: a gate is pending, or the
    /// loop is idle (parked with nothing queued). Mid-turn — running, no gate — is *not* settled.
    /// The predicate behind `/settle` and the `?wait` flag (docs/permissions.md).
    pub(super) fn is_settled(&self) -> bool {
        self.pending.lock().unwrap().is_some()
            || (!self.running.load(Ordering::SeqCst)
                && self.queued.load(Ordering::SeqCst) == 0
                // An accepted `POST /loop/continue` (docs/control-api.md) is a turn
                // about to run, exactly as a queued submission is.
                && !self.continue_pending())
    }

    /// Block until [`is_settled`]. Uses `Notified::enable` to register the waiter **before** the
    /// condition check: `settle_changed` is a `notify_waiters` (no stored permit), so a
    /// `parking`/`register_pending` that fires between the check and the await would otherwise be
    /// lost. With the waiter pre-registered, the wake is delivered to the pending future.
    ///
    /// [`is_settled`]: Shared::is_settled
    pub(super) async fn await_settled(&self) {
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

    pub(super) fn interrupt(&self) {
        // Latch first, then wake: a bash tool racing the abort future must never
        // resolve and reach the loop's juncture check before the flag is visible.
        self.interrupt_latch.store(true, Ordering::SeqCst);
        self.interrupt.notify_waiters();
    }

    /// Consume the latched interrupt — `true` exactly once per `POST /loop/interrupt`
    /// (docs/control-channel.md). The loop calls this at both junctures around a
    /// call: before one runs (it doesn't) and where its result lands (the result
    /// is withheld from the wire). Also cleared when a fresh user turn starts, so
    /// an interrupt fired at idle can't stop the *next* turn's first call.
    pub fn take_interrupt(&self) -> bool {
        self.interrupt_latch.swap(false, Ordering::SeqCst)
    }
}
