//! The zero-token continue — hand control back with nothing appended
//! (docs/control-api.md, docs/control-channel.md).
//!
//! `POST /loop/continue` asks the idle loop to take another turn on the wire it
//! already has: no `User` entry, no log append, no projection change. The signal
//! is a flag plus a wake, read by the agent loop's idle `select!` beside the user
//! turn and the notification wake — the same escalation shape the pinned-event
//! path uses (docs/supervisor.md), which also enters a trajectory with no new
//! user turn.
//!
//! Nothing here writes to the log, so the byte-stability pins cannot move: the
//! next request re-sends the same prefix it would have sent had control never
//! been handed back.

use super::*;
use crate::project::{ApiBlock, Role};

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
            Self::Empty => "nothing to continue: the session has no turns yet",
            Self::Completed => "nothing to continue: the last turn completed",
            Self::Orphaned => "nothing to continue: the last tool call has no result",
        }
    }
}

impl Session {
    /// Ask the idle loop to re-enter its trajectory with nothing appended, or say
    /// why it cannot (docs/control-api.md).
    ///
    /// Preconditions, in the order a client would want them named: no pending
    /// gate, the loop idle with nothing queued, and a **continuable tail**
    /// ([`continuable_tail`](Self::continuable_tail)). A continue already pending
    /// is idempotent — the loop has one to consume either way.
    pub(super) fn request_continue(&self) -> Result<(), NotContinuable> {
        if self.continue_pending.load(Ordering::SeqCst) {
            return Ok(());
        }
        if self.pending.lock().unwrap().is_some() {
            return Err(NotContinuable::Gated);
        }
        if !self.is_settled() {
            return Err(NotContinuable::Busy);
        }
        if let Some(why) = self.continuable_tail() {
            return Err(why);
        }
        self.continue_pending.store(true, Ordering::SeqCst);
        self.continue_signal.notify_waiters();
        // The daemon is no longer at a decision point, so a `?wait=settled`
        // long-poll must not answer with this instant.
        self.settle_changed.notify_waiters();
        self.touch_state();
        Ok(())
    }

    /// Whether re-prompting the current wire is meaningful — `None` when it is,
    /// the reason when it is not.
    ///
    /// The question is about the **wire**, so it is asked of the projection
    /// rather than of the log: record-only entries (an aborted attempt, a failed
    /// request, a `ConfigEdit`) project to nothing and so cannot make a finished
    /// turn look unfinished, and a `tool_result` tail is a user-role message
    /// exactly as a submitted turn is. A user-role tail means the model has not
    /// answered the last thing on the wire; an assistant-role tail means it has.
    fn continuable_tail(&self) -> Option<NotContinuable> {
        let messages = self.project_messages();
        let Some(last) = messages.last() else {
            return Some(NotContinuable::Empty);
        };
        match last.role {
            Role::User => None,
            Role::Assistant => Some(
                if last
                    .content
                    .iter()
                    .any(|block| matches!(block, ApiBlock::ToolUse { .. }))
                {
                    NotContinuable::Orphaned
                } else {
                    NotContinuable::Completed
                },
            ),
        }
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

    /// The loop took the continue: clear the flag and mark itself running, the
    /// way [`dequeued_turn`](Self::dequeued_turn) does for a submitted turn.
    /// Clearing is what keeps one gesture to one extra turn — a flag left set
    /// would re-continue at every return to idle.
    pub fn take_continue(&self) {
        self.running.store(true, Ordering::SeqCst);
        self.continue_pending.store(false, Ordering::SeqCst);
    }

    /// Whether a continue is waiting to be consumed — read by
    /// [`is_settled`](Self::is_settled), so the daemon does not report a decision
    /// point between the request and the loop picking it up.
    pub(super) fn continue_pending(&self) -> bool {
        self.continue_pending.load(Ordering::SeqCst)
    }
}
