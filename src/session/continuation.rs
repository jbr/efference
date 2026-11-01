//! The zero-token continue — hand control back with nothing appended
//! (docs/control-api.md, docs/control-channel.md).
//!
//! `POST /loop/continue` asks the idle loop to take another turn on the wire it
//! already has. The signal is a flag plus a wake, read by the agent loop's idle
//! `select!` beside the user turn and the notification wake — the same wake shape the
//! pinned-event path uses (docs/process-monitor.md).
//!
//! The one thing written is a `Telemetry::Continue` naming who sent it, which
//! projects to nothing, so the byte-stability pins cannot move.

use super::*;
use crate::record::log::ContinueOrigin;

/// Why a `POST /loop/continue` was declined (docs/control-api.md). The daemon
/// answers `409 not_continuable` carrying the message; a client renders it and
/// changes nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotContinuable {
    /// A turn is running, or one is queued and about to run — control is not the
    /// operator's to hand back.
    Busy,
    /// An approval gate is pending: the loop is waiting on a decision, not on a
    /// turn.
    Gated,
    /// An `ask` is pending: the loop is waiting on a reply, not on a turn.
    Asking,
    /// The log projects to nothing — there is no wire to re-prompt.
    Empty,
    /// The wire ends in a final assistant response: re-prompting identical bytes
    /// would spend a round-trip on a wire the model has already answered
    /// (docs/control-api.md).
    Completed,
    /// The wire ends in an assistant `tool_use` with no paired result — a shape
    /// the API rejects, so continuing could only fail.
    Orphaned,
}

impl NotContinuable {
    /// The human half of the decline, in the lead-with-what-happened register the
    /// other control-channel messages use.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Busy => "the loop is already running — there is nothing to hand back",
            Self::Gated => "an approval gate is pending — resolve it to continue",
            Self::Asking => "a question is pending — answer it with a turn to continue",
            Self::Empty => "nothing to continue: the session has no turns yet",
            Self::Completed => "nothing to continue: the last turn completed",
            Self::Orphaned => "nothing to continue: the last tool call has no result",
        }
    }
}

/// What an accepted continue does, decided by [`Session::continue_verdict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Continues {
    /// A continue is already waiting for the loop: the two are one.
    Already,
    /// The loop is parked at its cold check; the person's action the route notes is
    /// what wakes it.
    Release,
    /// The loop is waiting on the upstream — a pause between attempts, or a triage held
    /// without a verdict — and the continue retries now (docs/transport.md).
    RetryNow,
    /// The idle loop takes another turn on the wire it already has.
    Start,
}

impl Session {
    /// Whether `POST /loop/continue` accepts now, and what it would do — the route's
    /// whole precondition, which the `verbs` topic publishes (docs/control-api.md).
    ///
    /// Read off the loop phase, in the order a client would want the refusals named:
    /// a pending gate or question, then a loop doing something with nothing to retry,
    /// then a tail with no continue in it ([`continuable_tail`](Self::continuable_tail)).
    /// A continue already pending is idempotent — the loop has one to consume either way.
    ///
    /// The cases accepted while the loop is not idle are the loop parked at its cold
    /// check, and the loop **waiting on the upstream** — a backoff pause between
    /// attempts, or a triage held after its budget ran out. The same gesture that
    /// retries a stalled trajectory then retries that wait now (docs/transport.md), so
    /// the operator has one "try again" whichever side of the give-up they are on.
    ///
    /// The flag, the phase, and the queue come from one
    /// [`phase_and_queue`](Self::phase_and_queue), so a turn the loop is claiming reads as
    /// queued or running, never as an idle loop with nothing queued.
    pub(crate) fn continue_verdict(&self) -> Result<Continues, NotContinuable> {
        let now = self.phase_and_queue();
        if now.continue_pending {
            return Ok(Continues::Already);
        }
        match now.phase {
            LoopPhase::Gated => Err(NotContinuable::Gated),
            LoopPhase::Asking => Err(NotContinuable::Asking),
            LoopPhase::ColdParked => Ok(Continues::Release),
            LoopPhase::Blocked {
                reason: BlockedReason::TriageUnavailable | BlockedReason::TriageDefect,
            } => Ok(Continues::RetryNow),
            // A request between attempts, or re-sent after a pause, is still the retry
            // the topic names.
            LoopPhase::Delayed { .. } | LoopPhase::Requesting
                if self.upstream_retry.get().is_some() =>
            {
                Ok(Continues::RetryNow)
            }
            LoopPhase::Idle if !now.queued => match self.continuable_tail() {
                Some(why) => Err(why),
                None => Ok(Continues::Start),
            },
            _ => Err(NotContinuable::Busy),
        }
    }

    /// Ask the loop to re-enter its trajectory with nothing appended to its wire, or say
    /// why it cannot (docs/control-api.md). The verdict is read before the person's
    /// action is noted, since noting it releases a cold park the verdict reads. An
    /// accepted continue is recorded, with who sent it, before the loop is signalled, so
    /// it precedes the request it causes; a declined one writes nothing.
    pub(crate) fn request_continue(&self, origin: ContinueOrigin) -> Result<(), NotContinuable> {
        let verdict = self.continue_verdict();
        self.person_acted(PersonAction::Continue);
        let verdict = verdict?;
        let entry = Payload::Telemetry(crate::record::log::Telemetry::Continue(origin));
        // The record must not stop what it describes.
        if let Err(e) = self.append(self.latest_turn(), entry) {
            log::warn!("continue record: append failed: {e}");
        }
        match verdict {
            Continues::Already | Continues::Release => {}
            Continues::RetryNow => {
                self.upstream_retry.hurry();
                // A held triage's wait ends now (`phase.rs`); a pause between attempts
                // ends when the hurried wait returns.
                self.end_wait(
                    |phase| {
                        matches!(
                            phase,
                            LoopPhase::Blocked {
                                reason: BlockedReason::TriageUnavailable
                                    | BlockedReason::TriageDefect
                            }
                        )
                    },
                    LoopPhase::Requesting,
                );
            }
            Continues::Start => {
                // Under the queue lock, which `phase_and_queue` holds while it reads the
                // flag beside the phase, so the flag cannot change under it.
                {
                    let _queue = self.queue.lock().unwrap();
                    self.continue_pending.store(true, Ordering::SeqCst);
                }
                self.continue_signal.notify_waiters();
                // The daemon is no longer at a decision point, so a `?wait=settled`
                // long-poll must not answer with this instant.
                self.settle_changed.notify_waiters();
                self.touch_state();
            }
        }
        Ok(())
    }

    /// Whether re-prompting the current wire is meaningful — `None` when it is,
    /// the reason when it is not.
    ///
    /// The question is about the **wire**, so it is asked of the projection
    /// rather than of the log: record-only entries (an aborted attempt, a failed
    /// request, a `ConfigEdit`) project to nothing and so cannot make a finished
    /// turn look unfinished, and a `tool_result` tail is a user-role turn exactly
    /// as a submitted turn is. A user-role tail means the model has not answered
    /// the last thing on the wire; an assistant-role tail means it has, and
    /// re-prompting it is a request ending on the model's turn, which providers
    /// answer with an error, nothing, or a repeat (docs/transport.md).
    ///
    /// It reads the dialect-neutral fold, not one dialect's encoding: the Messages
    /// encoding drops Responses and Completions assistant turns, so on those
    /// sessions every tail would read as unanswered.
    fn continuable_tail(&self) -> Option<NotContinuable> {
        // A resumed session's pending batch ends the wire in unanswered `tool_use`
        // blocks, and the continue is what brings it back (docs/resumption.md): the
        // loop replays the batch before it builds a request.
        if self.pending_batch().is_some() {
            return None;
        }
        let turns = self.project_turns();
        let Some(last) = turns
            .iter()
            .rev()
            .find(|turn| !matches!(turn, WireTurn::Breakpoint))
        else {
            return Some(NotContinuable::Empty);
        };
        let WireTurn::Assistant(output) = last else {
            return None;
        };
        Some(match output.tool_calls() {
            Ok(calls) if calls.is_empty() => NotContinuable::Completed,
            _ => NotContinuable::Orphaned,
        })
    }

    /// The last live completion when the log ends at a pending batch — calls that never
    /// ran, or results awaiting their verdict (docs/resumption.md) — read from the log each
    /// time rather than from a flag.
    pub(crate) fn pending_batch(&self) -> Option<crate::record::recovery::Completion> {
        crate::record::recovery::pending_batch(self.log.lock().unwrap().entries())
    }

    /// Resolve when a continue is pending (docs/control-channel.md). Raced by the
    /// agent loop's idle `select!` against the next user turn and the inbox wake.
    ///
    /// Lost-wakeup safe in the same shape as [`await_settled`](Self::await_settled):
    /// the waiter is registered before the flag is read, so a `request_continue`
    /// landing between the read and the await still wakes this.
    pub async fn continue_wake(&self) {
        loop {
            let notified = self.continue_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.continue_pending.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    /// The loop took the continue: leave idle and clear the flag, both with the queue lock
    /// held, as [`claim_turn`](Self::claim_turn) takes a submitted turn — so
    /// [`phase_and_queue`](Self::phase_and_queue), which holds that lock, sees the continue
    /// pending or taken, never an idle loop with no continue pending in between.
    /// Clearing is what keeps one gesture to one extra turn — a flag left set
    /// would re-continue at every return to idle.
    pub fn take_continue(&self) {
        let _queue = self.queue.lock().unwrap();
        self.set_phase(LoopPhase::Requesting);
        self.continue_pending.store(false, Ordering::SeqCst);
    }

    /// Whether a continue is waiting to be consumed — read by
    /// [`phase_and_queue`](Self::phase_and_queue), so the daemon does not report a
    /// decision point between the request and the loop picking it up.
    pub(crate) fn continue_pending(&self) -> bool {
        self.continue_pending.load(Ordering::SeqCst)
    }
}
