//! The pending-notification queue (docs/inbox.md).

use super::*;

/// The pending-notification queue + its id counter (docs/inbox.md). Ids are
/// **queue-local**: an un-shown notification has no log `seq` yet, so it's addressed
/// by this id until the first-show append gives it a durable `seq`.
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
    /// Must-wake (docs/inbox.md): the loop pins this at its first show whatever the
    /// triage decides, so an idle agent gets a turn to act on it. Set by the
    /// harness for a delegated child's status, never by a client.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) wake: bool,
}

/// A notification taken off the pending queue for its first show: the record the
/// loop logs as an `Ambient`, and whether it must be pinned (docs/inbox.md).
pub struct Drained {
    pub notification: crate::log::Notification,
    pub wake: bool,
}

impl Session {
    /// Enqueue an ambient notification (docs/inbox.md — `POST /notifications`). If it names a
    /// `slot` (or an explicit `replaces` id) matching a pending entry, that entry is
    /// **superseded**: archived-and-replaced, never edited (records are immutable,
    /// docs/log-and-projection.md). A superseded-while-pending entry is simply dropped — it never reached a
    /// triage, so from the log's perspective it never happened. Returns the new id.
    pub(crate) fn notify(
        &self,
        source: String,
        body: String,
        slot: Option<String>,
        replaces: Option<u64>,
    ) -> u64 {
        self.enqueue(source, body, slot, replaces, false)
    }

    /// [`notify`](Self::notify) for a notification the agent must get a turn for
    /// (docs/inbox.md): the loop pins it at first show regardless of the triage's
    /// verdict. The harness's own channel — a delegated child's status
    /// (docs/tools.md) — not a client's.
    pub(crate) fn notify_wake(&self, source: String, body: String, slot: Option<String>) -> u64 {
        self.enqueue(source, body, slot, None, true)
    }

    fn enqueue(
        &self,
        source: String,
        body: String,
        slot: Option<String>,
        replaces: Option<u64>,
        wake: bool,
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
            wake,
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
    pub fn has_pending_notifications(&self) -> bool {
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
    pub(crate) fn dismiss_notification(&self, id: u64) -> bool {
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

    /// Drain the un-shown notifications for a triage (docs/inbox.md): take and clear
    /// the ephemeral queue. The loop logs each as a first-show `Ambient` entry (its
    /// `seq` becoming its durable id) and shows it to the model, so the queue holds
    /// only what's arrived since the last triage. Returned in arrival order, each a
    /// [`Notification`](crate::log::Notification) ready to wrap in a payload plus
    /// its must-wake flag, which the loop applies after the verdict.
    pub fn drain_inbox(&self) -> Vec<Drained> {
        let mut inbox = self.inbox.lock().unwrap();
        inbox
            .pending
            .drain(..)
            .map(|p| Drained {
                notification: crate::log::Notification {
                    source: p.source,
                    body: p.body,
                    slot: p.slot,
                },
                wake: p.wake,
            })
            .collect()
    }

    /// The directed inbox read (docs/inbox.md — `check_inbox`): the active shown
    /// notifications (left at earlier triages) plus the newly-arrived ephemeral ones,
    /// so the model can survey what's waiting on demand. The triage for this
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
