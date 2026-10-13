//! The `cold` state topic (docs/control-api.md, docs/transport.md): whether the
//! session's prompt cache is believed to have expired.
//!
//! The belief is a function of the clock — the last request on the main prefix plus
//! the provider's cache lifetime ([`believed_cold_since`]) — so nothing mutates when it
//! turns true. [`cold_watch`] re-reads it on a short period and pings the topic differ
//! when it moves; a request moving it back to warm is followed by an append, which
//! wakes the differ anyway.

use super::*;
use crate::log::{ColdEvent, ColdReason, ReleasedBy, Timestamp, UnparkCause};

/// Why the loop is stopped at its cold check mid-turn (the `park` topic,
/// docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParkReason {
    /// The cache is believed expired: continuing pays `context_tokens` uncached. The
    /// loop waits for a person to act (a turn, a continue, a decision, an answer), or a
    /// fold.
    Expired {
        since: Timestamp,
        context_tokens: Option<u64>,
    },
    /// A fold summarized the history while the loop was parked, so the belief is gone and
    /// the next turn or continue resumes the loop.
    Folded,
}

/// How often [`cold_watch`] re-reads the belief: the topic turns at most this late.
const COLD_CHECK: Duration = Duration::from_secs(15);

impl Session {
    /// Record the provider's cache lifetime (`Provider::cache_ttl`). Set once at boot,
    /// and only for a model with a prompt cache: without one every request is uncached,
    /// so there is nothing to keep and no cold state.
    pub(crate) fn set_cache_ttl(&self, ttl: Duration) {
        let _ = self.cache_ttl.set(ttl);
        self.touch_state();
    }

    /// From when the cache is believed expired, or `None` while it is believed warm,
    /// before the first request on the main prefix, and on a model with no cache.
    ///
    /// Only a request on the current prefix counts: after a fold nothing has sent the
    /// new one, which is the state of a session that has made no request at all.
    ///
    /// A session with nothing to fold is not cold either. Cold is a question put to the
    /// operator — pay the read, or fold — and where everything since the last fold
    /// fits in the verbatim tail there is only one answer, so nothing is asked.
    pub(crate) fn cold_since(&self) -> Option<Timestamp> {
        let ttl = *self.cache_ttl.get()?;
        let folded = self.last_fold_at();
        let last_touch = self
            .metrics
            .lock()
            .unwrap()
            .last_request_wall
            .filter(|touch| folded.is_none_or(|fold| touch.0 > fold.0));
        let since = believed_cold_since(last_touch, Timestamp::now(), ttl)?;
        let log = self.log.lock().unwrap();
        super::fold::fold_cut(log.entries(), super::fold::FOLD_TAIL_TOKENS)?;
        Some(since)
    }

    /// The `cold` topic: `{since, context_tokens}`, or `null`. `context_tokens` is
    /// the last turn's prompt — what reading the context unfolded would cost, in tokens.
    /// Whether the loop is stopped at its cold check is the separate `park` topic.
    /// A fold's progress is the separate `fold` topic.
    pub(super) fn cold_json(&self) -> Value {
        let Some(since) = self.cold_since() else {
            return Value::Null;
        };
        serde_json::json!({
            "since": since,
            "context_tokens": self.cold_context_tokens(),
        })
    }

    /// Why the loop is parked at its cold check, or `None` when it is not.
    pub(super) fn park_reason(&self) -> Option<ParkReason> {
        if !self.cold_parked() {
            return None;
        }
        Some(match self.cold_since() {
            Some(since) => ParkReason::Expired {
                since,
                context_tokens: self.cold_context_tokens(),
            },
            None => ParkReason::Folded,
        })
    }

    /// The `park` topic: `{reason: "expired", since, context_tokens}`,
    /// `{reason: "folded"}`, or `null` when the loop is not parked at its cold check.
    pub(super) fn park_json(&self) -> Value {
        match self.park_reason() {
            None => Value::Null,
            Some(ParkReason::Expired {
                since,
                context_tokens,
            }) => serde_json::json!({
                "reason": "expired",
                "since": since,
                "context_tokens": context_tokens,
            }),
            Some(ParkReason::Folded) => serde_json::json!({ "reason": "folded" }),
        }
    }

    /// The last turn's prompt in tokens: what reading the context unfolded would cost.
    pub(super) fn cold_context_tokens(&self) -> Option<u64> {
        self.metrics
            .lock()
            .unwrap()
            .last_turn_usage
            .as_ref()
            .map(Usage::prompt_tokens)
    }
}

impl Session {
    /// A person acted (docs/control-api.md): a turn, a continue, a decision at a gate,
    /// an answer to an `ask`. While the session is believed cold it is recorded and
    /// releases a loop parked at the cold check, or the next check the loop reaches, and
    /// lets a held inbox wake through; `by` is the action, kept in the log beside the
    /// park it releases. Nothing is left behind when the session is warm, so an action
    /// taken before a cold moment does not release a later one. A loop parked since
    /// before a fold, with the belief gone, is woken the same way.
    pub(crate) fn release_cold(&self, by: ReleasedBy) {
        if self.cold_since().is_some() {
            self.record_cold(ColdEvent::Released { by });
            self.person_acted.store(true, Ordering::SeqCst);
            self.cold_signal.notify_waiters();
        } else {
            self.wake_cold_check();
        }
    }

    /// Which action a turn is: an answer when an `ask` is pending (the turn resolves it
    /// instead of opening a user turn, `submit_turn`), otherwise a turn.
    pub(crate) fn turn_released_by(&self) -> ReleasedBy {
        if self.asking().is_some() {
            ReleasedBy::Ask
        } else {
            ReleasedBy::Turn
        }
    }

    /// Append one step of the cold check to the log (docs/control-api.md). A failed
    /// write is reported and dropped: the record must not fail what it describes.
    fn record_cold(&self, event: ColdEvent) {
        let turn = self.latest_turn();
        if let Err(e) = self.append(turn, Payload::Cold(event)) {
            eprintln!("[cold record: append failed: {e}]");
        }
    }

    /// Let a parked cold check look again: after a fold the session is no longer cold,
    /// and the operator's next turn or continue resumes it.
    pub(crate) fn wake_cold_check(&self) {
        if self.cold_parked() {
            self.cold_signal.notify_waiters();
        }
    }

    /// Hold until the cache is believed warm (docs/session-title.md):
    /// for a detached side task that would read the main prefix — the title ask —
    /// waiting is the polite move where the loop's own check would wait for a person.
    /// The session's next main request makes it warm and wakes the waiter; a task
    /// left waiting dies with the session's runtime.
    pub(crate) async fn wait_until_warm(&self) {
        loop {
            let notified = self.cold_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cold_since().is_none() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn cold_parked(&self) -> bool {
        self.cold_parked.load(Ordering::SeqCst)
    }

    /// The loop's cold check, before any request on the main prefix: returns at once
    /// while the cache is believed warm, consumes a person's action when it is not,
    /// and otherwise parks — a decision point, so `?wait` answers — until a person
    /// acts. An action left over from a cold moment that has since passed is dropped.
    pub(crate) async fn await_warm_or_person(&self) {
        loop {
            let notified = self.cold_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let believed_cold = self.cold_since();
            let Some(since) = believed_cold else {
                self.person_acted.store(false, Ordering::SeqCst);
                return self.unpark_cold(UnparkCause::Warm);
            };
            if self.person_acted.swap(false, Ordering::SeqCst) {
                return self.unpark_cold(UnparkCause::Person);
            }
            if !self.cold_parked.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "(cold — waiting for a person before reading the expired cache, or a fold)"
                );
                self.record_cold(ColdEvent::Parked {
                    reason: ColdReason::Expired,
                    since,
                    context_tokens: self.cold_context_tokens(),
                });
                self.settle_changed.notify_waiters();
                self.touch_state();
            }
            notified.await;
        }
    }

    fn unpark_cold(&self, cause: UnparkCause) {
        if self.cold_parked.swap(false, Ordering::SeqCst) {
            self.record_cold(ColdEvent::Unparked { cause });
            self.settle_changed.notify_waiters();
            self.touch_state();
        }
    }

    /// [`Self::inbox_wake`], held while the cache is believed cold: an idle triage is a
    /// request on the main prefix, so a notification waits in the inbox until a person
    /// acts (or a fold makes the session warm again).
    pub(crate) async fn inbox_wake_while_warm(&self) {
        loop {
            self.inbox_wake().await;
            if self.cold_since().is_none() {
                return;
            }
            self.cold_signal.notified().await;
        }
    }
}

/// Publish the `cold` topic's transitions: re-read the belief every [`COLD_CHECK`] and
/// signal the topic differ when it changes. Spawned beside the keepalive for a model
/// with a cache.
pub(crate) async fn cold_watch(session: Arc<Session>) {
    let mut last = session.cold_since();
    loop {
        tokio::time::sleep(COLD_CHECK).await;
        let now = session.cold_since();
        if now != last {
            session.touch_state();
            last = now;
        }
    }
}
