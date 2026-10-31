//! Operator pacing, the turn queue, the between-step juncture, and the interrupt
//! (docs/control-channel.md).

use super::*;

/// The operator's pacing state (docs/control-channel.md): a continuous speed dial
/// plus a pause **overlay**. `effective_sleep = held ? ∞ : speed` — `held` is an overlay, *not* a
/// speed value, so the dial retains `speed` while held and releasing returns to the retained speed,
/// never to zero. Freely adjustable while held and outliving it, so a change made during a pause
/// "recolors" what resume does.
#[derive(Default)]
pub(super) struct Pace {
    /// The between-turns delay when not held (`0` = as fast as possible, the default).
    pub(super) speed: Duration,
    /// The pause overlay: while set, the effective sleep is infinite (wait at the juncture until
    /// released). Retains `speed` underneath.
    pub(super) held: bool,
    /// Agent turns submitted while held, in submission order: in the queue, so they are shown
    /// and can be recalled, but not yet sent to the loop. Releasing the hold sends them.
    pub(super) held_back: Vec<SubmittedTurn>,
}

impl Pace {
    /// Drop the hold and hand the turns it held back to the loop, in submission order. Called
    /// with the pace lock held, so no turn is held back after the release it should have seen.
    fn release(&mut self, turns: &mpsc::UnboundedSender<SubmittedTurn>) {
        self.held = false;
        for turn in self.held_back.drain(..) {
            let _ = turns.send(turn);
        }
    }

    /// Whether `queue` holds a turn the loop will run: one not held back behind the hold.
    pub(super) fn turn_to_run_in(&self, queue: &std::collections::VecDeque<SubmittedTurn>) -> bool {
        queue.iter().any(|queued| {
            !self
                .held_back
                .iter()
                .any(|held| held.submission_id == queued.submission_id)
        })
    }
}

impl Session {
    pub(crate) fn submit_turn(&self, turn: SubmittedTurn) {
        // A pending `ask` takes the turn as its reply instead (docs/process-monitor.md). The
        // ask lock is held through the queue push below, so a turn cannot slip in between
        // an ask starting and recalling the queue.
        let mut ask = self.ask.lock().unwrap();
        if !turn.is_blank()
            && let Some(state) = ask.take()
        {
            drop(ask);
            self.resume_from_hold();
            self.deliver_reply(state, turn);
            return;
        }
        self.queue_turn(ask, turn);
    }

    /// Queue `turn` for the loop's next juncture, releasing the ask lock the caller holds
    /// once the turn is in the queue, so no ask can start between the check and the push.
    /// Returns whether the turn waits behind the operator's hold: an agent's turn
    /// ([`agent_source`](crate::frontend::agent_source)) submitted while held is queued but
    /// not sent until the hold is released (docs/control-channel.md). Any other turn
    /// releases the hold.
    ///
    /// The pace lock is taken before the push (ask, pace, queue: the order every path takes
    /// them in) and held through the hold's decision, so to
    /// [`phase_and_queue`](Self::phase_and_queue), which holds it, an agent's turn is queued
    /// and held back in one change. Pushed apart, a `settle` checking between the two reads
    /// the turn as one about to run and waits for a wake that holding it back never sends.
    pub(super) fn queue_turn(
        &self,
        ask: std::sync::MutexGuard<'_, Option<ask::AskState>>,
        turn: SubmittedTurn,
    ) -> bool {
        {
            let mut pace = self.pace.lock().unwrap();
            // Record the turn as queued *before* sending, so `is_settled` reads "busy" the
            // instant the turn is accepted — even before the loop dequeues it (the `/settle`
            // submit race, docs/permissions.md).
            self.queue.lock().unwrap().push_back(turn.clone());
            drop(ask);
            // Where a test lands a reader between the two pushes (`between_pushes`).
            #[cfg(test)]
            {
                let interleaved = self.between_pushes.lock().unwrap().take();
                if let Some(interleaved) = interleaved {
                    interleaved(self);
                }
            }
            if !crate::frontend::agent_source(turn.source.as_deref()) {
                pace.release(&self.turns_tx);
            } else if pace.held {
                pace.held_back.push(turn);
                drop(pace);
                self.touch_state();
                return true;
            }
            // The receiver lives in the loop's `RemoteFrontend`; a send fails only if
            // the loop has exited, in which case the daemon is shutting down anyway. Sent
            // under the pace lock, so it cannot overtake turns a concurrent release sends.
            let _ = self.turns_tx.send(turn);
        }
        self.end_pace_wait();
        self.pace_interrupt.notify_waiters();
        self.touch_state();
        false
    }

    /// An operator's submit resumes (docs/control-channel.md): drop any hold, send the agent
    /// turns it held back, and wake an in-flight juncture sleep so the message is delivered
    /// at the next juncture *immediately*, not after the remaining paced delay. The retained
    /// `speed` is untouched (it recolors later junctures); only the `held` overlay drops. Used
    /// by a turn that answers an `ask`; a queued turn does the same in [`Self::queue_turn`].
    pub(super) fn resume_from_hold(&self) {
        self.pace.lock().unwrap().release(&self.turns_tx);
        self.end_pace_wait();
        self.pace_interrupt.notify_waiters();
    }

    /// A submit or a release ends the juncture's pace wait, held or timed, at the moment
    /// it is taken (`phase.rs`).
    fn end_pace_wait(&self) {
        self.end_wait(
            |phase| {
                matches!(
                    phase,
                    LoopPhase::Delayed { .. }
                        | LoopPhase::Blocked {
                            reason: BlockedReason::PaceHeld
                        }
                )
            },
            LoopPhase::Requesting,
        );
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
    /// then drops it unappended. With `leaves_idle` (a turn the idle loop is about to run) the
    /// phase moves to `requesting` while the queue lock that covers the removal is held, so
    /// [`phase_and_queue`], which holds that lock, sees the turn queued or running and never an
    /// idle loop with an empty queue while a turn is in flight.
    ///
    /// [`phase_and_queue`]: Session::phase_and_queue
    pub(super) fn claim_turn(&self, submission_id: &str, leaves_idle: bool) -> bool {
        let claimed = {
            let mut q = self.queue.lock().unwrap();
            match q.iter().position(|t| t.submission_id == submission_id) {
                Some(pos) => {
                    if leaves_idle {
                        self.set_phase(LoopPhase::Requesting);
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
    pub(crate) fn set_pace(&self, speed: Option<Duration>, held: Option<bool>) -> (Duration, bool) {
        let mut wake = false;
        let result = {
            let mut p = self.pace.lock().unwrap();
            if let Some(s) = speed {
                p.speed = s;
            }
            if let Some(h) = held {
                if p.held && !h {
                    wake = true; // releasing a hold ends the juncture sleep now
                    p.release(&self.turns_tx);
                }
                p.held = h;
            }
            (p.speed, p.held)
        };
        if wake {
            self.end_pace_wait();
            self.pace_interrupt.notify_waiters();
        }
        self.touch_state();
        result
    }

    /// The interruptible between-turns sleep (docs/control-channel.md). Applied at the
    /// juncture — after a step's results land, before the next model call — and **transparent to the
    /// agent**: it touches no log entry, so a held/slowed loop projects byte-identically to a fast
    /// one. Waits `effective_sleep = held ? ∞ : speed`.
    ///
    /// The hold outranks queued input: only a release ends it — the operator's own submit, which
    /// releases it, or `pace` — so an agent's message cannot resume a session its operator
    /// paused. A paced delay is paced input only: it returns *immediately* if a turn is already
    /// queued and ends early when the `pace_interrupt` fires (a release or a submitted turn). A
    /// finite sleep that expires with no input just proceeds.
    ///
    /// Lost-wakeup safe: the interrupt future is armed (`enable`) *before* the state is read, so a
    /// release/submit racing the read still wakes the wait (mirrors [`await_settled`]).
    ///
    /// [`await_settled`]: Session::await_settled
    pub async fn pace_at_juncture(&self) {
        let mut interrupt = std::pin::pin!(self.pace_interrupt.notified());
        interrupt.as_mut().enable(); // register before reading state, so a racing wake isn't lost
        if self.pace_snapshot().1 {
            let _held = self.wait_phase(
                LoopPhase::Blocked {
                    reason: BlockedReason::PaceHeld,
                },
                LoopPhase::Requesting,
            );
            loop {
                interrupt.as_mut().await;
                interrupt.set(self.pace_interrupt.notified());
                interrupt.as_mut().enable();
                if !self.pace_snapshot().1 {
                    return;
                }
            }
        }
        // A submitted turn wins over a paced delay — deliver it now rather than pacing over it.
        if !self.queue.lock().unwrap().is_empty() {
            return;
        }
        let speed = self.pace_snapshot().0;
        if !speed.is_zero() {
            // Paced: proceed when the delay elapses or an interrupt (submit/release) ends it early.
            let until = crate::log::Timestamp::now().0 + speed.as_millis() as u64;
            let _delayed = self.wait_phase(LoopPhase::Delayed { until }, LoopPhase::Requesting);
            tokio::select! {
                _ = tokio::time::sleep(speed) => {}
                _ = interrupt => {}
            }
        }
        // Not held, no delay → proceed at once.
    }

    /// The idle loop woke for an inbox triage (docs/inbox.md): a request on the main
    /// prefix, so the loop is `requesting` until it either starts a trajectory
    /// or returns to idle with nothing pinned ([`Self::idle_triage_settled`]).
    pub(crate) fn idle_triage_started(&self) {
        self.set_phase(LoopPhase::Requesting);
    }

    /// An idle triage pinned nothing: back to idle without a turn having run, so the
    /// return is not a turn ending — no push, and no idle for a status subscriber.
    pub(crate) fn idle_triage_settled(&self) {
        self.set_phase(LoopPhase::Idle);
    }

    /// The loop is going idle (about to await the next user turn): move the phase to idle and
    /// wake any `/settle` waiter. If a turn is already queued, `recv` resolves at once and
    /// [`claim_turn`] moves the phase on again; meanwhile [`is_settled`] reads the idle phase
    /// with the queue as it stood at the same instant ([`phase_and_queue`]), so a turn
    /// submitted during the turn just ended reads as busy.
    ///
    /// [`claim_turn`]: Session::claim_turn
    /// [`is_settled`]: Session::is_settled
    /// [`phase_and_queue`]: Session::phase_and_queue
    pub(super) fn going_idle(&self) {
        // The phase left is what distinguishes the two ways the loop arrives here: a turn
        // just ended, or the daemon booted (or an idle triage pinned nothing) and no turn
        // ran. Only the first is worth a push (docs/web-push.md) — a settle notification
        // on boot idle would fire every time a session starts, saying nothing happened.
        // Only the loop moves the phase out of a step, so reading it before the move
        // cannot race. The count moves first, so whoever reads the idle reads the turn
        // it ended (`subscriptions.rs`).
        let a_turn_just_ended = *self.phase.lock().unwrap() != LoopPhase::Idle;
        if a_turn_just_ended {
            self.turns_ended.fetch_add(1, Ordering::SeqCst);
        }
        self.set_phase(LoopPhase::Idle);
        #[cfg(test)]
        self.loop_idled.store(true, Ordering::SeqCst);
        self.settle_changed.notify_waiters();
        // A queued turn is about to run, so the session is not waiting on anyone: no push
        // for an idle it passes straight through, as when an interrupt ends a turn with
        // a message already queued.
        if a_turn_just_ended && self.no_turn_queued_to_run() {
            // A supervised session's settle is its supervisor's to read; only one
            // without a live supervisor pushes the operator (docs/web-push.md).
            let body = push::settle_body(self.log.lock().unwrap().entries());
            self.moment(crate::log::MomentKind::Settle, self.supervisor(), body);
        }
    }

    /// Whether the loop has gone idle at least once (`loop_idled`).
    #[cfg(test)]
    pub(crate) fn loop_idled(&self) -> bool {
        self.loop_idled.load(Ordering::SeqCst)
    }

    /// Whether no queued turn is about to run: the queue is empty, or holds only agent turns
    /// held back behind the operator's hold, which wait on the operator as the idle loop
    /// does (docs/control-channel.md).
    fn no_turn_queued_to_run(&self) -> bool {
        let pace = self.pace.lock().unwrap();
        !pace.turn_to_run_in(&self.queue.lock().unwrap())
    }

    /// Whether a queued turn is about to run: [`turn_wake`](Self::turn_wake)'s condition.
    pub(crate) fn turn_to_run(&self) -> bool {
        !self.no_turn_queued_to_run()
    }

    /// Resolve when a queued turn is about to run — `wait`'s wake source beside notifications
    /// and the interrupt (docs/process-monitor.md). The juncture after the wait's result drains
    /// it, so the turn reaches the model right behind that result. An agent's turn held back
    /// behind the operator's hold does not count until the release sends it: the drain would
    /// not deliver it. Every path that sends a turn to the loop fires `pace_interrupt`; the
    /// waiter is armed before the check, so a send between the two still wakes it.
    pub(crate) async fn turn_wake(&self) {
        loop {
            let waiter = self.pace_interrupt.notified();
            if self.turn_to_run() {
                return;
            }
            waiter.await;
        }
    }

    /// The interrupt notifier, for a client whose reads should end on it
    /// (`Client::set_abort`, docs/control-channel.md).
    pub fn interrupt_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.interrupt)
    }

    /// Record where the interrupt came from, then signal it. The entry is appended
    /// first, so it precedes the `failed: interrupted` stage the signal causes in
    /// the log (docs/log-and-projection.md). A failed write is reported and
    /// dropped: the record must not stop what it describes.
    pub(crate) fn interrupt(&self, origin: crate::log::InterruptOrigin) {
        let entry = Payload::Telemetry(crate::log::Telemetry::Interrupt(origin));
        if let Err(e) = self.append(self.latest_turn(), entry) {
            log::warn!("interrupt record: append failed: {e}");
        }
        // Latch first, then wake: a bash tool racing the abort future must never
        // resolve and reach the loop's juncture check before the flag is visible.
        self.interrupt_latch.store(true, Ordering::SeqCst);
        self.interrupt.notify_waiters();
    }

    /// Who sent the latest interrupt, as its recorded `Telemetry::Interrupt` names them:
    /// `None` for the operator's own command line or TUI (docs/control-channel.md). The
    /// record is appended before the latch is set, so the loop reading the latch reads
    /// the record that set it.
    pub(crate) fn interrupter(&self) -> Option<String> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                Payload::Telemetry(crate::log::Telemetry::Interrupt(origin)) => {
                    Some(origin.source.clone())
                }
                _ => None,
            })
            .flatten()
    }

    /// What an interrupt stopped: a call that had run `elapsed`, with the `result` it
    /// left (docs/control-channel.md).
    pub(crate) fn interruption_after(
        &self,
        elapsed: Duration,
        result: &RawResult,
    ) -> crate::log::Interruption {
        crate::log::Interruption {
            by: self.interrupter(),
            after_ms: Some(elapsed.as_millis() as u64),
            killed: crate::bash::was_killed(result),
        }
    }

    /// Whether an interrupt is latched for the current call. The loop-owned `wait`
    /// checks this after arming the interrupt notifier, closing the interval where a
    /// notification could otherwise be missed before its wait future is polled.
    pub fn interrupt_pending(&self) -> bool {
        self.interrupt_latch.load(Ordering::SeqCst)
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
