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

impl Session {
    pub(super) fn submit_turn(&self, turn: SubmittedTurn) {
        // Record the turn as queued *before* sending, so `is_settled` reads "busy" the instant
        // the turn is accepted — even before the loop dequeues it (the `/settle` submit race,
        // docs/permissions.md).
        self.queue.lock().unwrap().push_back(turn.clone());
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
        self.touch_state();
    }

    /// The pending queue, in submission order — the `queued` state topic's source and the
    /// TUI/web pending echo's authority (docs/control-channel.md).
    pub(super) fn queued_turns(&self) -> Vec<SubmittedTurn> {
        self.queue.lock().unwrap().iter().cloned().collect()
    }

    /// Withdraw the **suffix** of the pending queue beginning at `submission_id`, returning what
    /// was actually popped, in submission order (docs/control-api.md — `DELETE /turns/:id`).
    ///
    /// A suffix, not an arbitrary delete: recalling an older turn means "pop up to and including
    /// that one", which is identical to popping repeatedly, so queue order is preserved for free
    /// and there is exactly one operation. The return value is what makes the race benign — the
    /// loop may have dequeued the turn already, in which case this pops fewer turns than asked
    /// (possibly none) and the client splices only what came back, never a message already sent.
    ///
    /// Nothing is logged: a pending turn was never appended, so a withdrawal has nothing to
    /// retract. A draft is not history.
    pub(super) fn withdraw_turns(&self, submission_id: &str) -> Vec<SubmittedTurn> {
        let popped: Vec<SubmittedTurn> = {
            let mut q = self.queue.lock().unwrap();
            match q.iter().position(|t| t.submission_id == submission_id) {
                Some(pos) => q.split_off(pos).into_iter().collect(),
                None => Vec::new(),
            }
        };
        if !popped.is_empty() {
            // Emptying the queue while the loop is parked makes the daemon settled, and the
            // `/settle` long-poll is edge-triggered on this.
            self.settle_changed.notify_waiters();
            self.touch_state();
        }
        popped
    }

    /// Claim a turn the loop just received off the channel: `true` when it is still pending (and
    /// it is removed from the queue), `false` when it was withdrawn while in flight — the caller
    /// then drops it unappended. `running` is set **before** the removal so [`is_settled`] never
    /// sees an empty queue and a not-running loop while a turn is in flight.
    ///
    /// [`is_settled`]: Session::is_settled
    pub(super) fn claim_turn(&self, submission_id: &str, mark_running: bool) -> bool {
        let claimed = {
            let mut q = self.queue.lock().unwrap();
            match q.iter().position(|t| t.submission_id == submission_id) {
                Some(pos) => {
                    if mark_running {
                        self.running.store(true, Ordering::SeqCst);
                    }
                    q.remove(pos);
                    true
                }
                None => false,
            }
        };
        self.touch_state();
        claimed
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
    /// [`await_settled`]: Session::await_settled
    pub async fn pace_at_seam(&self) {
        let interrupt = self.pace_interrupt.notified();
        tokio::pin!(interrupt);
        interrupt.as_mut().enable(); // register before reading state, so a racing wake isn't lost
        // A submitted turn always wins — deliver it now rather than pacing over it.
        if !self.queue.lock().unwrap().is_empty() {
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

    /// The loop is parking (about to await the next user turn): mark not-running and wake any
    /// `/settle` waiter. If a turn is already queued, `recv` resolves at once and [`claim_turn`]
    /// re-sets running, so a transient `running == false` with a non-empty queue still reads as
    /// busy.
    ///
    /// [`claim_turn`]: Session::claim_turn
    pub(super) fn parking(&self) {
        // The swap is what distinguishes the two ways the loop arrives here: a turn
        // just ended (`running` was true) or the daemon booted and has never run one.
        // Only the first is worth a push (docs/web-push.md) — a settle notification on
        // boot idle would fire every time a session starts, saying nothing happened.
        let a_turn_just_ended = self.running.swap(false, Ordering::SeqCst);
        self.settle_changed.notify_waiters();
        if a_turn_just_ended {
            let body = super::push::settle_body(self.log.lock().unwrap().entries());
            self.push("Message", body);
        }
    }

    /// Whether the daemon is at a **decision point** a client can act on: a gate is pending, or the
    /// loop is idle (parked with nothing queued). Mid-turn — running, no gate — is *not* settled.
    /// The predicate behind `/settle` and the `?wait` flag (docs/permissions.md).
    pub(super) fn is_settled(&self) -> bool {
        self.pending.lock().unwrap().is_some()
            || (!self.running.load(Ordering::SeqCst)
                && self.queue.lock().unwrap().is_empty()
                // An accepted `POST /loop/continue` (docs/control-api.md) is a turn
                // about to run, exactly as a queued submission is.
                && !self.continue_pending())
    }

    /// Block until [`is_settled`]. Uses `Notified::enable` to register the waiter **before** the
    /// condition check: `settle_changed` is a `notify_waiters` (no stored permit), so a
    /// `parking`/`register_pending` that fires between the check and the await would otherwise be
    /// lost. With the waiter pre-registered, the wake is delivered to the pending future.
    ///
    /// [`is_settled`]: Session::is_settled
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
