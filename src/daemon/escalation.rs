//! A moment's push held back while someone is attending (docs/web-push.md).
//!
//! When a moment fires (a gate opens, an `ask` is posed, a turn settles) and a
//! client is attending the host, the push is not sent: it is scheduled for
//! [`ESCALATION_DELAY`] later, and sent only if the moment is still standing then.
//! A moment stops standing when it **resolves** (the gate is decided, the `ask`
//! answered, a turn submitted, the session archived) or is **seen** (an active
//! client reports it is viewing this session, [`Host::report_attention`]). Both
//! call [`Session::cancel_escalation`].
//!
//! A session holds at most one pending escalation. A newer moment supersedes an
//! older one: the older moment's own claim on the person is covered by the newer
//! one, whose push names the session just the same.

use super::*;
use crate::log::{AttentiveClient, MomentKind, PushCancel, PushEvent};

/// How long an attended moment waits before it pushes. One value for every kind
/// of moment: long enough for a person at the screen to act on a gate or read a
/// settle, short enough that stepping away costs a couple of minutes rather than
/// the moment.
pub(crate) const ESCALATION_DELAY: Duration = Duration::from_secs(120);

/// A scheduled push and the task that will send it.
pub(super) struct Escalation {
    moment: u64,
    kind: MomentKind,
    message: crate::push::Message,
    task: tokio::task::AbortHandle,
}

/// The slot is shared with the timer task so the task can claim its own
/// escalation without holding the session.
pub(super) type EscalationSlot = Arc<Mutex<Option<Escalation>>>;

impl Session {
    /// Hold `message` for [`ESCALATION_DELAY`], then send it unless cancelled.
    /// With no runtime there is no timer to run, so nothing is scheduled (the
    /// same contexts in which a send does nothing). A pending older escalation
    /// is cancelled as superseded.
    pub(super) fn schedule_escalation(
        &self,
        moment: u64,
        kind: MomentKind,
        message: crate::push::Message,
        attentive: Vec<AttentiveClient>,
    ) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        self.cancel_escalation(PushCancel::Superseded);
        self.record_push(
            moment,
            kind,
            PushEvent::Held {
                delay_secs: ESCALATION_DELAY.as_secs(),
                attentive,
            },
        );
        let slot = Arc::clone(&self.escalation);
        let me = self.pushes.me();
        let task = handle
            .spawn({
                let slot = Arc::clone(&slot);
                async move {
                    tokio::time::sleep(ESCALATION_DELAY).await;
                    // Claim by task identity: a cancel that raced the timer has
                    // already emptied or replaced the slot, and then this fires
                    // nothing.
                    let id = tokio::task::id();
                    let claimed = slot.lock().unwrap().take_if(|e| e.task.id() == id);
                    if let Some(escalation) = claimed
                        && let Some(session) = me.upgrade()
                    {
                        session.record_push(
                            escalation.moment,
                            escalation.kind,
                            PushEvent::Escalated,
                        );
                        session.send_in_background(
                            escalation.moment,
                            escalation.kind,
                            escalation.message,
                        );
                    }
                }
            })
            .abort_handle();
        let previous = slot.lock().unwrap().replace(Escalation {
            moment,
            kind,
            message,
            task,
        });
        if let Some(previous) = previous {
            previous.task.abort();
        }
    }

    /// Drop the pending escalation, if any, recording why: the moment it was
    /// waiting to announce has resolved or been seen. Cheap enough to call from
    /// any resolution site unconditionally; nothing is recorded when nothing was
    /// pending.
    pub(crate) fn cancel_escalation(&self, cause: PushCancel) {
        let taken = self.escalation.lock().unwrap().take();
        if let Some(escalation) = taken {
            escalation.task.abort();
            self.record_push(
                escalation.moment,
                escalation.kind,
                PushEvent::Cancelled { cause },
            );
        }
    }

    /// The pending escalation's message, for tests.
    #[cfg(test)]
    pub(crate) fn pending_escalation(&self) -> Option<crate::push::Message> {
        self.escalation
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.message.clone())
    }
}

#[cfg(test)]
mod test;
