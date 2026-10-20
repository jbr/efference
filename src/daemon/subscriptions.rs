//! Subscriptions: one session's status reaching other sessions' inboxes
//! (docs/tools.md).
//!
//! The subscriber set is host control state on the target — never logged, never
//! projected. `create` from a session subscribes the creator with `wake`, so the
//! parent gets a turn at each of the child's decision points except a gate routed to
//! the operator, and a supervisor set after creation is subscribed on the same terms
//! (`host/supervise.rs`); any other subscription is ambient, and the subscriber's
//! triage may dismiss it. The host runs one watcher per session that reads the status
//! at every control-state change and posts it to the subscribers when it moves.

use super::host::Host;
use super::status::{Condition, Reader, Relation, Reviewer, SessionStatus, render, status_slot};
use super::*;

/// One subscriber's terms: whether the status must wake it (docs/inbox.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub(crate) wake: bool,
}

impl Host {
    /// Subscribe `subscriber` to `target`'s status. `false` when either is not
    /// hosted. Re-subscribing never lowers the terms: a creator's `wake`
    /// survives an ambient subscription, and only `unsubscribe` drops it.
    pub fn subscribe(&self, target: &str, subscriber: &str, wake: bool) -> bool {
        let Some(session) = self.session(subscriber) else {
            return false;
        };
        let mut sessions = self.sessions_mut();
        let Some(hosted) = sessions.hosted_mut(target) else {
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
            .hosted_mut(target)
            .is_some_and(|hosted| hosted.subscribers.remove(subscriber).is_some());
        if removed && let Some(session) = self.session(subscriber) {
            session.subscription_removed();
        }
        removed
    }

    /// `target`'s subscribers and their terms, sorted by id.
    pub(crate) fn subscribers(&self, target: &str) -> Vec<(String, Subscription)> {
        self.sessions_mut()
            .hosted(target)
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
    /// subscriber that has ended is skipped. Each body is the one status view rendered
    /// for its reader (`status/render.rs`): a `wake` subscriber reads it as a driver,
    /// any other as an observer.
    ///
    /// A gate is the one status that is more than observation. **Who answers it is the
    /// review-level dial's to say, not the subscription's** (docs/permissions.md): a
    /// session whose position resolves to `supervisor` sends the call it is holding to
    /// its supervisor, subscribed or not, with the wake a held call warrants and the
    /// body a reviewer reads, which names the moves that decide it. That reviewer is
    /// skipped in the subscriber loop so the two posts do not race on one slot. Every
    /// other status, and every other position, posts to subscribers alone.
    ///
    /// A gate the dial routes to the operator asks the operator, so it reaches a `wake`
    /// subscriber without the wake. The session's supervisor may still decide it, and
    /// its body names the `decide` call (`Relation::MayDecide`).
    ///
    /// A question that returned a session's queued message is that session's news as
    /// well (docs/process-monitor.md): a sender that is not subscribed is woken with the
    /// status, which tells it its message came back. Only a driver's message is queued,
    /// so it reads as a driver.
    pub(super) fn post_status(&self, id: &str, session: &Session, status: &SessionStatus) {
        let view = session.status_view(id, status);
        // The view read the routing once; the post follows that same reading.
        let reviewer = match &view.condition {
            Condition::Gated(held) => match &held.reviewer {
                Reviewer::Supervisor { id } => Some(id.clone()),
                Reviewer::Operator => None,
            },
            _ => None,
        };
        let now = crate::log::Timestamp::now().0;
        // The notification's source is the session's id, and the pinned block reads
        // `⟢ <source>: <body>`, so the status opens without the id it would repeat.
        let body = |relation, reader: &str| {
            let text = render(&view, &Reader::notification(relation, reader), now);
            match text.strip_prefix(&format!("{id}: ")) {
                Some(rest) => rest.to_string(),
                None => text,
            }
        };
        let subscribers = self.subscribers(id);
        let returned_to: Vec<String> = match &view.condition {
            Condition::Asking { returned, .. } => returned
                .iter()
                .filter_map(|message| crate::frontend::source_session(message.source.as_deref()?))
                .filter(|sender| {
                    !subscribers
                        .iter()
                        .any(|(subscriber, _)| subscriber == sender)
                        && reviewer.as_deref() != Some(*sender)
                })
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        for sender in returned_to {
            if let Some(inbox) = self.session(&sender) {
                let body = body(Relation::Driver, &sender);
                inbox.notify_wake(id.to_string(), body, Some(status_slot(id)));
            }
        }
        let to_operator = matches!(
            &view.condition,
            Condition::Gated(held) if held.reviewer == Reviewer::Operator
        );
        let supervisor = self.supervisor_of(id);
        for (subscriber, terms) in subscribers {
            if Some(&subscriber) == reviewer.as_ref() {
                continue;
            }
            let Some(inbox) = self.session(&subscriber) else {
                continue;
            };
            let supervises = supervisor.as_deref() == Some(subscriber.as_str());
            let relation = Relation::of_session(&view, &subscriber, supervises, terms.wake);
            let body = body(relation, &subscriber);
            if terms.wake && !to_operator {
                inbox.notify_wake(id.to_string(), body, Some(status_slot(id)));
            } else {
                inbox.notify(id.to_string(), body, Some(status_slot(id)));
            }
        }
        if let Some(reviewer) = reviewer
            && let Some(inbox) = self.session(&reviewer)
        {
            inbox.notify_wake(
                id.to_string(),
                body(Relation::Reviewer, &reviewer),
                Some(status_slot(id)),
            );
        }
    }

    /// Forget every subscription `id` holds on other sessions, and release its
    /// own subscribers' hold on it — it has ended, so it can wake nobody. The
    /// subscribers' `wait` source counts drop with it (docs/process-monitor.md).
    pub(super) fn drop_subscriptions_of(&self, id: &str) {
        let subscribers = {
            let mut sessions = self.sessions_mut();
            for hosted in sessions.hosted_values_mut() {
                hosted.subscribers.remove(id);
            }
            sessions
                .hosted_mut(id)
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
    /// child not created `detached`, a child still starting included.
    pub(super) fn cascade_of(&self, id: &str) -> Vec<String> {
        let sessions = self.sessions_mut();
        let hosted = sessions
            .hosted_iter()
            .filter(|(_, hosted)| hosted.created_by.as_deref() == Some(id) && !hosted.detached)
            .map(|(child, _)| child.clone());
        let starting = sessions
            .starting_iter()
            .filter(|starting| starting.created_by() == Some(id) && !starting.detached())
            .map(|starting| starting.id().to_string());
        hosted.chain(starting).collect()
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
/// The resolved review position and the live supervisor are part of the level, not just
/// the status, because together they decide where a gate is *routed*
/// (docs/permissions.md). A held call whose routing moves — the dial turned to hand the
/// child back, the supervisor set to another session or cleared, or the supervisor
/// ending — is re-routed rather than dropped: the status has not changed, so only the
/// routing carries the news.
pub(super) async fn watch_status(host: std::sync::Weak<Host>, id: String, session: Arc<Session>) {
    let mut last: Option<(SessionStatus, Routing)> = None;
    // How many trajectories had ended at the last idle this watcher read: an idle is
    // news when one has ended since, and not when an idle triage that pinned nothing
    // passed through `running` and back without a turn having run.
    let mut idle_seen = session.turns_ended();
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
        // Read after the status: the count moves before the phase reaches idle
        // (`going_idle`), so an idle read here already counts the turn it ended.
        let ended = session.turns_ended();
        let routing = (session.resolved_review_level(), session.supervisor());
        let notable = match (&last, &status) {
            (_, SessionStatus::Running) => false,
            (None, SessionStatus::Idle) => false,
            (Some(_), SessionStatus::Idle) => ended != idle_seen,
            (Some((previous, routed)), current) => {
                previous != current
                    || (matches!(current, SessionStatus::Gated { .. }) && *routed != routing)
            }
            (None, _) => true,
        };
        if let Some(host) = host.upgrade() {
            if notable {
                host.post_status(&id, &session, &status);
            }
            // The listing carries a status and a process count per row
            // (`listing.rs`), and both move with the control state this loop
            // already wakes on — so every wake pings it. Subscribers diff for
            // themselves, so a wake that moved nothing costs a comparison.
            host.touch_listing();
        }
        if status == SessionStatus::Idle {
            idle_seen = ended;
        }
        last = Some((status, routing));

        tokio::select! {
            _ = &mut settled => {}
            _ = &mut changed => {}
            _ = &mut appended => {}
        }
    }
}

/// Where a gate is routed: the resolved review position and the live supervisor.
type Routing = (crate::policy::ReviewLevel, Option<String>);

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
