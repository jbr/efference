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

/// How long an attended moment waits before it pushes. One value for every kind
/// of moment: long enough for a person at the screen to act on a gate or read a
/// settle, short enough that stepping away costs a couple of minutes rather than
/// the moment.
pub(crate) const ESCALATION_DELAY: Duration = Duration::from_secs(120);

/// A scheduled push and the task that will send it.
pub(super) struct Escalation {
    message: crate::push::Message,
    task: tokio::task::AbortHandle,
}

/// The slot is shared with the timer task so the task can claim its own
/// escalation without holding the session.
pub(super) type EscalationSlot = Arc<Mutex<Option<Escalation>>>;

impl Session {
    /// Hold `message` for [`ESCALATION_DELAY`], then send it unless cancelled.
    /// With no runtime there is no timer to run, so nothing is scheduled (the
    /// same contexts in which a send does nothing).
    pub(super) fn schedule_escalation(&self, message: crate::push::Message) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let slot = Arc::clone(&self.escalation);
        let task = handle
            .spawn({
                let slot = Arc::clone(&slot);
                async move {
                    tokio::time::sleep(ESCALATION_DELAY).await;
                    // Claim by task identity: a cancel that raced the timer has
                    // already emptied or replaced the slot, and then this fires
                    // nothing.
                    let me = tokio::task::id();
                    let claimed = slot.lock().unwrap().take_if(|e| e.task.id() == me);
                    if let Some(escalation) = claimed {
                        super::push::send_in_background(escalation.message);
                    }
                }
            })
            .abort_handle();
        let previous = slot.lock().unwrap().replace(Escalation { message, task });
        if let Some(previous) = previous {
            previous.task.abort();
        }
    }

    /// Drop the pending escalation, if any: the moment it was waiting to announce
    /// has resolved or been seen. Cheap enough to call from any resolution site
    /// unconditionally.
    pub(crate) fn cancel_escalation(&self) {
        if let Some(escalation) = self.escalation.lock().unwrap().take() {
            escalation.task.abort();
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
