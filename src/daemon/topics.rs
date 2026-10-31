//! Noticing that a state topic changed (docs/control-api.md).
//!
//! The topics sit behind ~10 independent mutexes and atomics with no change
//! notification of their own. Rather than a notify per mutation site, there is **one**
//! `state_changed` ping and **one** task that recomputes every topic and diffs it against
//! what it last published. Three consequences, and they are the reason for the shape:
//!
//! - It computes **once for all clients**, and only when something moved — not once per
//!   connected client per poll interval.
//! - A **missed ping degrades to "late", not to "stale forever and silently"**: the task
//!   recomputes *all* topics on any wake, so the next ping from any source repairs a topic
//!   whose own mutation site forgot to signal. That matters because three topics are
//!   mutated through `Arc`s the mutating code holds *without* a [`Session`] — the
//!   upstream-failure cell (the HTTP client), the cost cell (a spawned fetch), and the
//!   retry cell (either retry loop) — so they cannot ping `state_changed`. The first two
//!   move in lockstep with requests, and a request is followed by an append, so waking on
//!   `appended` covers them. The retry cell is the exception that carries its own signal:
//!   a retry pause has no append to ride on and lasts up to thirty seconds, so "late"
//!   would mean "wrong for the entire time the state is true".
//! - `GET /state` and the `state` frames come from the **same per-topic functions**
//!   ([`Session::topic_json`]), so the fold-equals-snapshot invariant holds by
//!   construction rather than by two implementations agreeing.
//!
//! Per-topic notifies are an optimization available later; nothing here forecloses them.

use super::*;

/// One topic's new value, ready to send.
///
/// Carries the topic's **entire** current value, never a patch: replacement is idempotent
/// and order-insensitive, which is what lets a backlogged connection coalesce two updates
/// to one topic into the later one, and what makes the fold-equals-`GET /state` identity
/// true regardless of delivery order.
#[derive(Clone, Debug)]
pub(crate) struct StateUpdate {
    pub(crate) topic: Topic,
    /// The serialized value. `Arc` because every connection gets the same bytes.
    pub(crate) json: Arc<str>,
    /// The log position this value reflects — an LSN-style read position, not an epoch
    /// (there is one writer and nothing is being fenced). A connection releases this
    /// update once it has delivered every log frame through the watermark, which is what
    /// makes "the state you hold is true as of the last log frame you received" a
    /// property rather than a hope. `None` on an empty log: nothing to wait for.
    pub(crate) watermark: Option<u64>,
}

impl Session {
    /// Signal that some state topic may have moved. Cheap and idempotent — the reader
    /// recomputes everything, so an over-eager ping costs one diff and a spurious ping
    /// costs nothing.
    pub(crate) fn touch_state(&self) {
        self.state_changed.notify_waiters();
    }

    /// Subscribe to state-topic updates. Every `GET /events` connection takes one of
    /// these; the sender lives as long as [`Session`], so a subscriber outlives any
    /// particular publisher.
    pub(crate) fn subscribe_state(&self) -> broadcast::Receiver<StateUpdate> {
        self.state_tx.subscribe()
    }

    /// Every topic's current value, each stamped with the log position it reflects.
    ///
    /// **Read the values first, then `last_seq`.** That ordering is load-bearing: it
    /// guarantees the watermark is at least as new as everything the values reflect. An
    /// append landing in between makes the stamp *newer* than the values, which only
    /// delays release. Reversed, an append landing in between would let an update claim
    /// to be older than it is, and a client would render state that already accounts for
    /// an entry it has not been sent.
    fn topic_snapshot(&self) -> Vec<StateUpdate> {
        let values: Vec<(Topic, Arc<str>)> = Topic::ALL
            .iter()
            .map(|&topic| (topic, Arc::from(self.topic_json(topic).to_string())))
            .collect();
        let watermark = self.last_committed_seq();
        values
            .into_iter()
            .map(|(topic, json)| StateUpdate {
                topic,
                json,
                watermark,
            })
            .collect()
    }

    /// The current value of each named topic, stamped with one shared watermark.
    ///
    /// Values first, then `last_seq` — the same ordering [`topic_snapshot`] uses and for
    /// the same reason: a stamp that runs ahead of its values only delays release, while
    /// one that lags them would let a client render state accounting for an entry it has
    /// not been sent.
    ///
    /// [`topic_snapshot`]: Session::topic_snapshot
    pub(super) fn subscribed_snapshot(&self, topics: &[Topic]) -> Vec<StateUpdate> {
        let values: Vec<(Topic, Arc<str>)> = topics
            .iter()
            .map(|&topic| (topic, Arc::from(self.topic_json(topic).to_string())))
            .collect();
        let watermark = self.last_committed_seq();
        values
            .into_iter()
            .map(|(topic, json)| StateUpdate {
                topic,
                json,
                watermark,
            })
            .collect()
    }
}

/// The single diffing task: wake, recompute every topic, publish the ones that moved.
///
/// Spawned once per session by the runtime, and holds an `Arc` for the daemon's lifetime like
/// the keepalive and config-watch loops do — the session ends when the runtime returns.
pub(crate) async fn watch_topics(shared: Arc<Session>) {
    // What each topic looked like when last published. A topic absent here has never been
    // published, so its first computed value counts as a change — which is what gives a
    // freshly-connected client a full snapshot.
    let mut published: HashMap<Topic, Arc<str>> = HashMap::new();
    loop {
        // Arm every wait BEFORE reading, so a change landing between the read and the
        // await cannot be lost — the same lost-wakeup discipline the log tail uses.
        //
        // Three signals rather than one, because the daemon already had two of them and
        // duplicating a ping beside an existing `notify_waiters` would be a second thing
        // to remember at each site: `appended` covers the log-derived topics (and the
        // telemetry that always accompanies a request), `settle_changed` covers the gate
        // appearing and the loop phase moving — `gate` and `loop_phase` exactly — and
        // `state_changed` is for the mutations with no existing signal of their own.
        let changed = shared.state_changed.notified();
        let appended = shared.appended.notified();
        let settled = shared.settle_changed.notified();
        // The fourth signal, and the one that is not a `Session` field's own: the
        // retry cell is written from the triage, which holds a `Client` and no
        // `Session`, and a retry pause is not followed by an append — so neither
        // fallback above would repair it before the pause ended.
        let retried = shared.upstream_retry.changed();

        for update in shared.topic_snapshot() {
            if published
                .get(&update.topic)
                .is_some_and(|previous| *previous == update.json)
            {
                continue;
            }
            published.insert(update.topic, Arc::clone(&update.json));
            // A send with no subscribers is dropped, which is correct: with no client
            // connected there is nobody to tell, and the next subscriber is sent a full
            // snapshot from `Session` rather than from this history.
            let _ = shared.state_tx.send(update);
        }

        tokio::select! {
            _ = changed => {}
            _ = appended => {}
            _ = settled => {}
            _ = retried => {}
        }
    }
}

#[cfg(test)]
mod test;
