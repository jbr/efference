//! The pending-notification queue (docs/inbox.md).

use super::*;

/// The pending-notification queue + its id counter (docs/inbox.md). Ids are
/// **queue-local**: an un-shown notification has no log `seq` yet, so it's addressed
/// by this id until Slice C's first-show append gives it a durable `seq`.
#[derive(Default)]
pub(super) struct Inbox {
    pub(super) next_id: u64,
    pub(super) pending: Vec<PendingNotification>,
}

/// One pending ambient notification, surfaced by `GET /state` (docs/inbox.md). Immutable
/// once enqueued — supersession archives-and-replaces rather than editing (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize)]
pub(super) struct PendingNotification {
    /// Queue-local id; the handle `DELETE /notifications/:id` addresses.
    pub(super) id: u64,
    pub(super) source: String,
    pub(super) body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) slot: Option<String>,
}

impl Shared {
    /// Enqueue an ambient notification (docs/inbox.md — `POST /notifications`). If it names a
    /// `slot` (or an explicit `replaces` id) matching a pending entry, that entry is
    /// **superseded**: archived-and-replaced, never edited (records are immutable,
    /// docs/log-and-projection.md). A superseded-while-pending entry is simply dropped — it never reached a
    /// gate, so from the log's perspective it never happened. Returns the new id.
    pub(super) fn notify(
        &self,
        source: String,
        body: String,
        slot: Option<String>,
        replaces: Option<u64>,
    ) -> u64 {
        let mut inbox = self.inbox.lock().unwrap();
        // Supersede: an explicit `replaces` id, and/or any pending entry on the slot.
        if let Some(old) = replaces {
            inbox.pending.retain(|n| n.id != old);
        }
        if let Some(s) = &slot {
            inbox
                .pending
                .retain(|n| n.slot.as_deref() != Some(s.as_str()));
        }
        let id = inbox.next_id;
        inbox.next_id += 1;
        inbox.pending.push(PendingNotification {
            id,
            source,
            body,
            slot,
        });
        drop(inbox);
        // Wake an idle agent for a standalone triage (docs/inbox.md — wake-on-notification).
        self.notified.notify_waiters();
        self.touch_state();
        id
    }

    /// Whether any **un-shown** notification is queued (the ephemeral pending set).
    /// The idle wake triggers on this — never on already-shown "left" items, so a
    /// deferred notification can't spin the idle loop (docs/inbox.md).
    pub(super) fn has_pending_notifications(&self) -> bool {
        !self.inbox.lock().unwrap().pending.is_empty()
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

    /// Dismiss a pending notification out-of-band (docs/inbox.md —
    /// `DELETE /notifications/:id`). `false` → no pending notification with that id.
    pub(super) fn dismiss_notification(&self, id: u64) -> bool {
        let removed = {
            let mut inbox = self.inbox.lock().unwrap();
            let before = inbox.pending.len();
            inbox.pending.retain(|n| n.id != id);
            inbox.pending.len() != before
        };
        if removed {
            self.touch_state();
        }
        removed
    }

    /// The current pending inbox, in arrival order — surfaced by `GET /state`.
    pub(super) fn pending_notifications(&self) -> Vec<PendingNotification> {
        self.inbox.lock().unwrap().pending.clone()
    }

    /// Drain the un-shown notifications for a triage gate (docs/inbox.md): take and clear
    /// the ephemeral queue. The loop logs each as a first-show `Ambient` entry (its
    /// `seq` becoming its durable id) and shows it to the model, so the queue holds
    /// only what's arrived since the last gate. Returned in arrival order as
    /// [`Notification`](crate::log::Notification)s ready to wrap in a payload.
    pub fn drain_inbox(&self) -> Vec<crate::log::Notification> {
        let mut inbox = self.inbox.lock().unwrap();
        inbox
            .pending
            .drain(..)
            .map(|p| crate::log::Notification {
                source: p.source,
                body: p.body,
                slot: p.slot,
            })
            .collect()
    }

    /// The directed inbox read (docs/inbox.md — `check_inbox`): the active shown
    /// notifications (left at earlier gates) plus the newly-arrived ephemeral ones,
    /// so the model can survey what's waiting on demand. The fork peek for this
    /// tool's own result is where they're actually triaged.
    pub fn inbox_summary(&self) -> String {
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
            out.push_str(&format!("\n(new) {} · {}", n.source, n.body));
        }
        out.push_str("\n(triage them in this step's review.)");
        out
    }
}
