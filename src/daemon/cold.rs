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
    pub(crate) fn cold_since(&self) -> Option<Timestamp> {
        let ttl = *self.cache_ttl.get()?;
        let last_touch = self.metrics.lock().unwrap().last_request_wall;
        believed_cold_since(last_touch, Timestamp::now(), ttl)
    }

    /// The `cold` topic: `{since, context_tokens}`, or `null`. `context_tokens` is the
    /// last turn's prompt — what reading the context unfolded would cost, in tokens.
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
        })
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
