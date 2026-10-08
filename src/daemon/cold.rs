//! The `cold` state topic (backlog: cold-session.md, docs/transport.md): whether the
//! session's prompt cache is believed to have expired.
//!
//! The belief is a function of the clock — the last request on the main prefix plus
//! the provider's cache lifetime ([`believed_cold_since`]) — so nothing mutates when it
//! turns true. [`cold_watch`] re-reads it on a short period and pings the topic differ
//! when it moves; a request moving it back to warm is followed by an append, which
//! wakes the differ anyway.

use super::*;
use crate::log::Timestamp;

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

    /// The `cold` topic: `{since, context_tokens, fold}`, or `null`. `context_tokens` is
    /// the last turn's prompt — what reading the context unfolded would cost, in tokens;
    /// `fold` is a fold in progress or the last one's failure.
    pub(super) fn cold_json(&self) -> Value {
        let Some(since) = self.cold_since() else {
            return Value::Null;
        };
        let context_tokens = self
            .metrics
            .lock()
            .unwrap()
            .last_turn_usage
            .as_ref()
            .map(Usage::prompt_tokens);
        serde_json::json!({
            "since": since,
            "context_tokens": context_tokens,
            "fold": self.fold_state(),
        })
    }
}

impl Session {
    /// Consent to one cold read (`accept_cold_read`, docs/control-api.md). Recorded only
    /// while the session is believed cold, so a client that sends the flag on every
    /// turn leaves nothing behind for a later cold moment it never saw; wakes a loop
    /// parked at the cold check and a held inbox wake.
    pub(crate) fn grant_cold_read(&self) {
        if self.cold_since().is_some() {
            self.cold_consent.store(true, Ordering::SeqCst);
            self.cold_signal.notify_waiters();
        }
    }

    /// Let a parked cold check look again, without consent: after a fold the session
    /// is no longer cold, and the operator's next turn or continue resumes it.
    pub(crate) fn wake_cold_check(&self) {
        if self.cold_parked() {
            self.cold_signal.notify_waiters();
        }
    }

    /// Hold until the cache is believed warm, without consent (docs/session-title.md):
    /// for a detached side task that would read the main prefix — the title ask —
    /// waiting is the polite move where the loop's own check would ask the operator.
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
    /// while the cache is believed warm, consumes a granted consent when it is not,
    /// and otherwise parks — a decision point, so `?wait` answers — until consent
    /// arrives. Consent left over from a cold moment that has since passed is dropped.
    pub(crate) async fn await_warm_or_consent(&self) {
        loop {
            let notified = self.cold_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cold_since().is_none() {
                self.cold_consent.store(false, Ordering::SeqCst);
                return self.unpark_cold();
            }
            if self.cold_consent.swap(false, Ordering::SeqCst) {
                return self.unpark_cold();
            }
            if !self.cold_parked.swap(true, Ordering::SeqCst) {
                eprintln!("(cold — waiting for consent to read the expired cache, or a fold)");
                self.settle_changed.notify_waiters();
                self.touch_state();
            }
            notified.await;
        }
    }

    fn unpark_cold(&self) {
        if self.cold_parked.swap(false, Ordering::SeqCst) {
            self.settle_changed.notify_waiters();
            self.touch_state();
        }
    }

    /// [`Self::inbox_wake`], held while the cache is believed cold: an idle triage is a
    /// request on the main prefix, so a notification waits in the inbox until a turn
    /// with consent (or a fold) makes the session warm again.
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
