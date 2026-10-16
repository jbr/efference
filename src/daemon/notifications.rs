//! The notification queue and the pending view (docs/inbox.md).

use super::*;
use crate::log::{Notification, Seq};

/// Notifications that wait in memory: those bound to a `slot`, which a later one on the
/// same slot supersedes, so none of them is logged until a triage first shows it
/// (docs/inbox.md). Every other notification is a log entry from its arrival.
#[derive(Default)]
pub(super) struct Inbox {
    pub(super) pending: Vec<Notification>,
}

/// One pending ambient notification, as `GET /state` surfaces it (docs/inbox.md): an
/// arrived notification not yet shown by a triage, or a slot notification still queued.
#[derive(Clone, Debug, Serialize)]
pub(super) struct PendingNotification {
    /// The arrival entry's `seq`, the handle a triage's verdict addresses. Absent for a
    /// slot notification, which has no entry until it is shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) seq: Option<Seq>,
    pub(super) source: String,
    pub(super) body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) slot: Option<String>,
    /// Must-wake (docs/inbox.md): the loop pins this at its first show whatever the
    /// triage decides, so an idle agent gets a turn to act on it. Set by the
    /// harness for a delegated child's status, never by a client.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) wake: bool,
}

impl Session {
    /// File an ambient notification (docs/inbox.md — `POST /notifications`). One without
    /// a slot is appended to the log as its `Ambient` entry now, and the answer is that
    /// entry's `seq`. One bound to a `slot` waits in memory and **supersedes** any queued
    /// one on the same slot — archived-and-replaced, never edited (records are immutable,
    /// docs/log-and-projection.md); it reaches the log only when a triage first shows it,
    /// and the answer is `None`.
    pub(crate) fn notify(&self, source: String, body: String, slot: Option<String>) -> Option<Seq> {
        self.enqueue(Notification::new(source, body, slot))
    }

    /// [`notify`](Self::notify) for a notification the agent must get a turn for
    /// (docs/inbox.md): the loop pins it at first show regardless of the triage's
    /// verdict. The harness's own channel — a delegated child's status
    /// (docs/tools.md) — not a client's.
    pub(crate) fn notify_wake(
        &self,
        source: String,
        body: String,
        slot: Option<String>,
    ) -> Option<Seq> {
        self.enqueue(Notification::new(source, body, slot).waking())
    }

    fn enqueue(&self, notification: Notification) -> Option<Seq> {
        let mut seq = None;
        let queued = if notification.slot.is_some() {
            Some(notification)
        } else {
            match self.append(self.latest_turn(), Payload::Ambient(notification.clone())) {
                Ok(arrived) => {
                    seq = Some(arrived);
                    None
                }
                Err(e) => {
                    // The log refused the write; keep the notification in memory so it
                    // is logged when a triage shows it, as a slot notification is.
                    log::error!("log: appending a notification at arrival failed: {e}");
                    Some(notification)
                }
            }
        };
        if let Some(n) = queued {
            let mut inbox = self.inbox.lock().unwrap();
            if let Some(s) = &n.slot {
                inbox
                    .pending
                    .retain(|q| q.slot.as_deref() != Some(s.as_str()));
            }
            inbox.pending.push(n);
        }
        // Wake an idle agent for a standalone triage (docs/inbox.md — wake-on-notification).
        self.notified.notify_waiters();
        self.touch_state();
        seq
    }

    /// Whether any notification waits for a first show: an arrival no triage has shown, or
    /// a queued slot notification. The idle wake triggers on this — never on already-shown
    /// "left" items, so a deferred notification can't spin the idle loop (docs/inbox.md).
    pub fn has_pending_notifications(&self) -> bool {
        !self.inbox.lock().unwrap().pending.is_empty() || {
            let log = self.log.lock().unwrap();
            crate::inbox::has_unshown(log.entries())
        }
    }

    /// Resolve when there's an un-shown notification to triage (docs/inbox.md). Lost-wakeup
    /// safe: arm the waiter before the check, so a `notify` landing between the check
    /// and the await still wakes us (the `appended`-tail pattern). The agent loop's
    /// idle `select!` races this against the next user turn.
    pub async fn inbox_wake(&self) {
        loop {
            let waiter = self.notified.notified();
            if self.has_pending_notifications() {
                return;
            }
            waiter.await;
        }
    }

    /// The current pending inbox, in arrival order — surfaced by `GET /state`: the
    /// arrivals no triage has shown, then the queued slot notifications.
    pub(super) fn pending_notifications(&self) -> Vec<PendingNotification> {
        let unshown = {
            let log = self.log.lock().unwrap();
            crate::inbox::unshown(log.entries())
        };
        let queued = self.inbox.lock().unwrap().pending.clone();
        unshown
            .into_iter()
            .map(|n| PendingNotification {
                seq: Some(n.seq),
                source: n.source,
                body: n.body,
                slot: None,
                wake: n.wake,
            })
            .chain(queued.into_iter().map(|n| PendingNotification {
                seq: None,
                source: n.source,
                body: n.body,
                slot: n.slot,
                wake: n.wake,
            }))
            .collect()
    }

    /// Take the queued slot notifications for a triage (docs/inbox.md): clear the
    /// in-memory queue, in arrival order. The caller appends each as its `Ambient`
    /// entry, which is its first show; the arrivals already in the log need no such step.
    pub fn drain_inbox(&self) -> Vec<Notification> {
        std::mem::take(&mut self.inbox.lock().unwrap().pending)
    }

    /// The directed inbox read (docs/inbox.md — `check_inbox`): the active shown
    /// notifications (left at earlier triages) plus the newly-arrived ephemeral ones,
    /// so the model can survey what's waiting on demand. The triage for this
    /// tool's own result is where they're actually triaged.
    pub fn inbox_listing(&self) -> String {
        let pending = self.pending_notifications();
        let active = {
            let log = self.log.lock().unwrap();
            crate::inbox::active_shown(log.entries())
        };
        if pending.is_empty() && active.is_empty() {
            return "Inbox empty — no pending notifications.".to_string();
        }
        let mut out = format!(
            "Inbox: {} pending notification(s).",
            pending.len() + active.len()
        );
        for n in &active {
            out.push_str(&format!("\n[{}] {} · {}", n.seq, n.source, n.body));
        }
        for n in &pending {
            match n.seq {
                Some(seq) => out.push_str(&format!("\n[{seq}] (new) {} · {}", n.source, n.body)),
                None => out.push_str(&format!("\n(new) {} · {}", n.source, n.body)),
            }
        }
        out.push_str("\n(triage them in this step's review.)");
        out
    }
}
