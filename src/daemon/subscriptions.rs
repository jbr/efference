//! Subscriptions: one session's status reaching other sessions' inboxes
//! (docs/tools.md).
//!
//! The subscriber set is host control state on the target — never logged, never
//! projected. `create` from a session subscribes the creator with `wake`, so the
//! parent gets a turn at each of the child's decision points; an explicit
//! `subscribe` is ambient, and the subscriber's triage may dismiss it. The host
//! runs one watcher per session that reads the status at every control-state
//! change and posts it to the subscribers when it moves.

use super::host::Host;
use super::status::{SessionStatus, status_slot};
use super::*;

/// One subscriber's terms: whether the status must wake it (docs/inbox.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub(crate) wake: bool,
}

impl Host {
    /// Subscribe `subscriber` to `target`'s status. `false` when either is not
    /// hosted. Re-subscribing never lowers the terms: a creator's `wake`
    /// survives an explicit `subscribe`, and only `unsubscribe` drops it.
    pub fn subscribe(&self, target: &str, subscriber: &str, wake: bool) -> bool {
        let Some(session) = self.session(subscriber) else {
            return false;
        };
        let mut sessions = self.sessions_mut();
        let Some(hosted) = sessions.get_mut(target) else {
            return false;
        };
        match hosted.subscribers.entry(subscriber.to_string()) {
            std::collections::btree_map::Entry::Occupied(mut existing) => {
                existing.get_mut().wake |= wake;
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Subscription { wake });
                session.subscription_added();
            }
        }
        true
    }

    /// Drop `subscriber`'s subscription to `target`. `false` when there was none.
    pub fn unsubscribe(&self, target: &str, subscriber: &str) -> bool {
        let removed = self
            .sessions_mut()
            .get_mut(target)
            .is_some_and(|hosted| hosted.subscribers.remove(subscriber).is_some());
        if removed && let Some(session) = self.session(subscriber) {
            session.subscription_removed();
        }
        removed
    }

    /// `target`'s subscribers and their terms, sorted by id.
    pub(crate) fn subscribers(&self, target: &str) -> Vec<(String, Subscription)> {
        self.sessions_mut()
            .get(target)
            .map(|hosted| {
                hosted
                    .subscribers
                    .iter()
                    .map(|(id, terms)| (id.clone(), *terms))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Post `status` of `id` to each subscriber's inbox, on the session's slot so
    /// a pending status is superseded rather than stacked (docs/inbox.md). A
    /// subscriber that has ended is skipped.
    ///
    /// A gate is the one status that is more than observation. **Who answers it is the
    /// review-level dial's to say, not the subscription's** (docs/permissions.md): a
    /// session whose position resolves to `upstream` sends the call it is holding to
    /// its creator, subscribed or not, with the wake a held call warrants and a body
    /// that says the recipient is the one to resolve it. That reviewer is skipped in
    /// the subscriber loop so the two posts do not race on one slot. Every other
    /// status, and every other position, posts to subscribers alone.
    pub(super) fn post_status(&self, id: &str, session: &Session, status: &SessionStatus) {
        let body = session.status_body(id, status);
        let reviewer = self.gate_reviewer(session, status);
        for (subscriber, terms) in self.subscribers(id) {
            if Some(&subscriber) == reviewer.as_ref() {
                continue;
            }
            let Some(inbox) = self.session(&subscriber) else {
                continue;
            };
            if terms.wake {
                inbox.notify_wake(id.to_string(), body.clone(), Some(status_slot(id)));
            } else {
                inbox.notify(id.to_string(), body.clone(), Some(status_slot(id)), None);
            }
        }
        if let Some(reviewer) = reviewer
            && let Some(inbox) = self.session(&reviewer)
        {
            inbox.notify_wake(
                id.to_string(),
                session.upstream_gate_body(id, status),
                Some(status_slot(id)),
            );
        }
    }

    /// The session this status is *routed to*, as against merely reported: the live
    /// upstream of a session holding a gate whose position resolves to `upstream`
    /// (docs/permissions.md). `None` for every other status and position — including a
    /// session whose creator has ended, whose gate the operator answers.
    fn gate_reviewer(&self, session: &Session, status: &SessionStatus) -> Option<String> {
        if !matches!(status, SessionStatus::Gated { .. })
            || session.resolved_review_level() != crate::policy::ReviewLevel::Upstream
        {
            return None;
        }
        session.upstream()
    }

    /// Forget every subscription `id` holds on other sessions, and release its
    /// own subscribers' hold on it — it has ended, so it can wake nobody. The
    /// subscribers' `wait` source counts drop with it (docs/process-monitor.md).
    pub(super) fn drop_subscriptions_of(&self, id: &str) {
        let subscribers = {
            let mut sessions = self.sessions_mut();
            for hosted in sessions.values_mut() {
                hosted.subscribers.remove(id);
            }
            sessions
                .get_mut(id)
                .map(|hosted| std::mem::take(&mut hosted.subscribers))
                .unwrap_or_default()
        };
        for subscriber in subscribers.keys() {
            if let Some(session) = self.session(subscriber) {
                session.subscription_removed();
            }
        }
    }

    /// The sessions `id` commissioned that end with it (docs/tools.md): every
    /// child not created `detached`.
    pub(super) fn cascade_of(&self, id: &str) -> Vec<String> {
        self.sessions_mut()
            .iter()
            .filter(|(_, hosted)| hosted.created_by.as_deref() == Some(id) && !hosted.detached)
            .map(|(child, _)| child.clone())
            .collect()
    }

    /// Every session `id` commissioned, detached or not — the sessions whose
    /// `upstream` review position resolves through `id` (docs/permissions.md).
    pub(super) fn children_of(&self, id: &str) -> Vec<String> {
        self.sessions_mut()
            .iter()
            .filter(|(_, hosted)| hosted.created_by.as_deref() == Some(id))
            .map(|(child, _)| child.clone())
            .collect()
    }
}

/// Watch one session's status and post each move to its subscribers. Runs under
/// the session's runtime `Swansong`, so it stops with the session; the end
/// itself is posted by the adopt watcher once the runtime has returned.
///
/// Level-triggered on the current status: a gate that resolves before this
/// wakes is not reported, since there is nothing left to act on. The boot idle
/// is not reported either — only an idle a turn led to.
///
/// The resolved review position is part of the level, not just the status, because it
/// decides where a gate is *routed* (docs/permissions.md). A held call whose position
/// moves — the dial turned to hand the child back, or the upstream ending — is
/// re-routed rather than dropped: the status has not changed, so only the position
/// carries the news.
pub(super) async fn watch_status(host: std::sync::Weak<Host>, id: String, session: Arc<Session>) {
    let mut last: Option<(SessionStatus, crate::policy::ReviewLevel)> = None;
    loop {
        // Arm before reading, so a change landing between the read and the await
        // cannot be lost — the same discipline the topic differ uses.
        let settled = session.settle_changed.notified();
        let changed = session.state_changed.notified();
        let appended = session.appended.notified();
        tokio::pin!(settled);
        tokio::pin!(changed);
        tokio::pin!(appended);
        settled.as_mut().enable();
        changed.as_mut().enable();
        appended.as_mut().enable();

        let status = session.status();
        let level = session.resolved_review_level();
        let notable = match (&last, &status) {
            (_, SessionStatus::Running) => false,
            (None, SessionStatus::Idle) => false,
            (Some((previous, routed)), current) => {
                previous != current
                    || (matches!(current, SessionStatus::Gated { .. }) && *routed != level)
            }
            (None, _) => true,
        };
        if let Some(host) = host.upgrade() {
            if notable {
                host.post_status(&id, &session, &status);
            }
            // The listing carries a status and a process count per row
            // (`summary.rs`), and both move with the control state this loop
            // already wakes on — so every wake pings it. Subscribers diff for
            // themselves, so a wake that moved nothing costs a comparison.
            host.touch_summary();
        }
        last = Some((status, level));

        tokio::select! {
            _ = &mut settled => {}
            _ = &mut changed => {}
            _ = &mut appended => {}
        }
    }
}

impl Session {
    /// Whether this session is subscribed to any other (docs/tools.md) — a source
    /// that may wake it, as the `wait` tool counts them (docs/process-monitor.md).
    pub fn has_subscriptions(&self) -> bool {
        self.subscriptions.load(Ordering::SeqCst) > 0
    }

    pub(super) fn subscription_added(&self) {
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) fn subscription_removed(&self) {
        let _ = self
            .subscriptions
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            });
    }
}
