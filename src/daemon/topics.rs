//! Noticing that a state topic changed (docs/control-api.md).
//!
//! The topics sit behind ~10 independent mutexes and atomics with no change
//! notification of their own. Rather than a notify per mutation site, there is **one**
//! `state_changed` ping and **one** task that recomputes every topic and diffs it against
//! what it last published. Three consequences, and they are the reason for the shape:
//!
//! - It computes **once for all clients** — strictly less work than the per-client 400 ms
//!   `GET /state` round-trip it replaces, and only when something moved.
//! - A **missed ping degrades to "late", not to "stale forever and silently"**: the task
//!   recomputes *all* topics on any wake, so the next ping from any source repairs a topic
//!   whose own mutation site forgot to signal. That matters because two topics are mutated
//!   through `Arc`s the mutating code holds *without* a [`Shared`] — the upstream-failure
//!   cell (the HTTP client) and the cost cell (a spawned fetch) — so they cannot ping.
//!   Both move in lockstep with requests, and a request is followed by an append, so
//!   waking on `appended` as well as on `state_changed` covers them.
//! - `GET /state` and the `state` frames come from the **same per-topic functions**
//!   ([`Shared::topic_json`]), so the fold-equals-snapshot invariant holds by
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

impl Shared {
    /// Signal that some state topic may have moved. Cheap and idempotent — the reader
    /// recomputes everything, so an over-eager ping costs one diff and a spurious ping
    /// costs nothing.
    pub(crate) fn touch_state(&self) {
        self.state_changed.notify_waiters();
    }

    /// Subscribe to state-topic updates. Every `GET /events` connection takes one of
    /// these; the sender lives as long as [`Shared`], so a subscriber outlives any
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
}

/// The single diffing task: wake, recompute every topic, publish the ones that moved.
///
/// Spawned once per daemon by `serve`, and holds an `Arc` for the daemon's lifetime like
/// the keepalive and config-watch loops do — the process ends when `serve` returns.
pub(crate) async fn watch_topics(shared: Arc<Shared>) {
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
        // appearing and the loop parking — `gate` and `running` exactly — and
        // `state_changed` is for the mutations with no existing signal of their own.
        let changed = shared.state_changed.notified();
        let appended = shared.appended.notified();
        let settled = shared.settle_changed.notified();

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
            // snapshot from `Shared` rather than from this history.
            let _ = shared.state_tx.send(update);
        }

        tokio::select! {
            _ = changed => {}
            _ = appended => {}
            _ = settled => {}
        }
    }
}

#[cfg(test)]
mod test;
