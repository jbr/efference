//! Transport diagnostics shared between the HTTP client and `GET /state`
//! (docs/transport.md).
//!
//! Three cells, all written from inside the client and read by the daemon:
//!
//! - [`UpstreamFailureState`] — the last non-success status and its consecutive count.
//!   Content-free by design; the bodies live in the log.
//! - [`UpstreamRetryState`] — a retry in flight, level-triggered: which surface, which
//!   attempt, of what budget, and when the next attempt fires. Carries its own change
//!   signal because the triage writes it from a code path that holds no `Session`.
//! - [`UpstreamAttemptQueue`] — failed HTTP attempts the retry handler absorbed, waiting
//!   for the loop to drain them into durable record-only log entries. The handler sits
//!   below the loop and cannot append, so the body crosses that boundary here.
//!
//! None of these is projected. The queue's *contents* reach the log, but as
//! record-only entries that project to nothing (docs/log-and-projection.md).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::sync::futures::Notified;

/// The upstream's sticky-routing token for the user turn in flight
/// (docs/transport.md). The Codex backend issues one on the first response of a
/// turn and expects it replayed on every later request of that turn; a new user
/// turn starts empty. Written by the response handler, read when a request is
/// built, cleared by the agent loop. Never logged: it is routing state, not a
/// diagnostic, and it is opaque.
pub type TurnRoutingState = Arc<Mutex<Option<String>>>;

/// Construct an empty turn-routing cell.
pub fn turn_routing_state() -> TurnRoutingState {
    Arc::new(Mutex::new(None))
}

/// The latest upstream failure: optional HTTP status, consecutive occurrences,
/// and error detail. Cleared on HTTP 2xx; later stream failure sets it again.
/// Ephemeral transport diagnostics: never appended
/// to the log or projected (docs/transport.md).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamFailure {
    pub status: Option<u16>,
    pub count: u32,
    pub detail: String,
}

impl UpstreamFailure {
    pub fn new(status: Option<u16>, detail: impl Into<String>) -> Self {
        Self {
            status,
            count: 1,
            detail: detail.into(),
        }
    }
}

/// Shared state written by the HTTP client and read through the daemon's
/// `GET /state` view (docs/transport.md).
pub type UpstreamFailureState = Arc<Mutex<Option<UpstreamFailure>>>;

/// Construct an empty upstream-failure diagnostic cell.
pub fn upstream_failure_state() -> UpstreamFailureState {
    Arc::new(Mutex::new(None))
}

/// Which loop is retrying. A triage retry and a turn retry have different
/// consequences — a withheld triage asks the agent to re-run a call whose side
/// effects already stand, where an exhausted turn ends the trajectory — so the
/// surface is part of the state rather than a detail folded into prose
/// (docs/transport.md).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrySurface {
    Turn,
    Triage,
}

impl RetrySurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Triage => "triage",
        }
    }
}

/// A retry in flight: what a client renders while the loop is waiting out an
/// aborted stream (docs/transport.md).
///
/// `next_attempt_at` is an **absolute** instant rather than a countdown, so the
/// daemon never has to re-emit a ticking field — a client computes the remaining
/// wait itself (docs/control-api.md). It is control state, never projected, so the
/// wall-clock reading cannot reach the cached prefix.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamRetry {
    pub surface: RetrySurface,
    /// Which attempt just failed — 1-based, so `attempt` of `budget` reads directly.
    pub attempt: u32,
    /// Total attempts the loop will make before it gives up.
    pub budget: u32,
    /// Unix millis at which the next attempt fires.
    pub next_attempt_at: u64,
    /// Leads with what happened and what is being done about it — *stream aborted,
    /// retrying (2 of 12)*. The status code and the upstream's own words are
    /// `detail`, not the headline.
    pub message: String,
    /// The upstream's error text for the attempt that failed.
    pub detail: String,
}

/// The retry cell, its change signal, and the operator's "retry now".
///
/// The signal is here rather than on `Session` because the triage publishes retry
/// state from inside `triage`, which holds a `Client` and no `Session` — and unlike
/// the upstream-failure cell, a retry is *not* followed by an append, so the
/// daemon's wake-on-`appended` fallback would leave the topic late for the whole
/// pause (daemon/topics.rs). "Retry now" lives here for the same reason: the waits it
/// cuts short run in both loops, and only this cell reaches both.
#[derive(Default, Debug)]
pub struct UpstreamRetryCell {
    state: Mutex<Option<UpstreamRetry>>,
    changed: Notify,
    /// Set by [`hurry`](Self::hurry), consumed by the wait it ends — a flag beside
    /// the notify so a hurry landing between a publish and the wait that follows it
    /// is not lost.
    hurried: AtomicBool,
    hurry: Notify,
    /// When the loop's own wait between attempts ends (absolute Unix millis), while
    /// one lasts: the `delayed` loop phase (docs/control-api.md). Set by
    /// [`delay`](Self::delay) only, so a detached aside retrying on the same cell
    /// does not read as the loop waiting.
    delaying: Mutex<Option<u64>>,
}

/// Shared retry state: written by both retry loops, read by `GET /state`.
pub type UpstreamRetryState = Arc<UpstreamRetryCell>;

/// Construct an empty retry cell.
pub fn upstream_retry_state() -> UpstreamRetryState {
    Arc::new(UpstreamRetryCell::default())
}

impl UpstreamRetryCell {
    /// The retry in flight, or `None` when nothing is being retried.
    pub fn get(&self) -> Option<UpstreamRetry> {
        self.state.lock().unwrap().clone()
    }

    /// Publish a retry. Signals only on an actual change, so a re-set of an
    /// identical value costs no wake.
    pub fn set(&self, retry: UpstreamRetry) {
        let mut state = self.state.lock().unwrap();
        if state.as_ref() == Some(&retry) {
            return;
        }
        *state = Some(retry);
        drop(state);
        self.changed.notify_waiters();
    }

    /// The all-clear for one surface: the attempt that finalizes, or exhaustion.
    ///
    /// Scoped to the surface so a triage's clear cannot erase a turn's live retry.
    /// The two surfaces do not in fact run concurrently (the triage is awaited inline
    /// from the loop), which is what makes the scoping cheap insurance rather than a
    /// coordination mechanism.
    pub fn clear(&self, surface: RetrySurface) {
        let mut state = self.state.lock().unwrap();
        if state.as_ref().is_none_or(|retry| retry.surface != surface) {
            return;
        }
        *state = None;
        drop(state);
        // A hurry aimed at the retry that just ended has nothing left to cut short.
        self.hurried.store(false, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    /// The operator's "retry now" (docs/transport.md): end the current wait — a
    /// backoff pause, or a hold after the budget ran out — so the next attempt
    /// fires immediately. The caller decides whether anything is waiting; a hurry
    /// with no wait in progress is consumed by the next wait to start, or dropped
    /// when that retry clears.
    pub fn hurry(&self) {
        self.hurried.store(true, Ordering::SeqCst);
        self.hurry.notify_waiters();
    }

    /// Wait out `pause`, or until [`hurry`](Self::hurry) — with `None`, only until
    /// a hurry. Returns whether the wait was cut short.
    pub async fn pause(&self, pause: Option<Duration>) -> bool {
        let hurried = self.hurry.notified();
        tokio::pin!(hurried);
        // Registered before the flag is read, so a hurry between the two wakes this.
        hurried.as_mut().enable();
        if self.hurried.swap(false, Ordering::SeqCst) {
            return true;
        }
        let elapsed = async {
            match pause {
                Some(pause) => tokio::time::sleep(pause).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = elapsed => false,
            () = &mut hurried => {
                self.hurried.store(false, Ordering::SeqCst);
                true
            }
        }
    }

    /// [`pause`](Self::pause) for `pause`, as the loop's own wait between attempts:
    /// [`delaying`](Self::delaying) names when it ends for as long as it lasts, which
    /// is what the `delayed` loop phase reads (docs/control-api.md). The end is the
    /// published retry's `next_attempt_at`, so the phase and the retry topic agree.
    pub async fn delay(&self, pause: Duration) -> bool {
        let until = self.get().map_or_else(
            || crate::log::Timestamp::now().0 + pause.as_millis() as u64,
            |retry| retry.next_attempt_at,
        );
        *self.delaying.lock().unwrap() = Some(until);
        self.changed.notify_waiters();
        // Cleared on every exit, a dropped wait included, so the phase never outlives it.
        let _clear = DelayEnds(self);
        self.pause(Some(pause)).await
    }

    /// When the loop's wait between attempts ends, while one lasts.
    pub fn delaying(&self) -> Option<u64> {
        *self.delaying.lock().unwrap()
    }

    /// Wait for the next change. Arm this **before** reading the value, like every
    /// other lost-wakeup-sensitive wait in the daemon.
    pub fn changed(&self) -> Notified<'_> {
        self.changed.notified()
    }
}

/// Clears [`UpstreamRetryCell::delaying`] when the wait it marks ends, however it ends.
struct DelayEnds<'a>(&'a UpstreamRetryCell);

impl Drop for DelayEnds<'_> {
    fn drop(&mut self) {
        *self.0.delaying.lock().unwrap() = None;
        self.0.changed.notify_waiters();
    }
}

/// One failed HTTP attempt, carrying the upstream's own words.
///
/// Produced by the client handler that observes each response and consumed by the
/// agent loop, which appends it to the log as a record-only entry. Between those two
/// points it holds no timestamp: the log stamps the entry when it lands, and nothing
/// here reaches a projection (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamAttemptFailure {
    pub status: u16,
    pub body: String,
}

impl UpstreamAttemptFailure {
    /// The text of the record-only log entry this becomes. Leads with what
    /// happened; the status is a detail inside it.
    pub fn record_text(&self) -> String {
        format!(
            "upstream attempt failed (HTTP {}): {}",
            self.status, self.body
        )
    }
}

/// How many undrained attempt failures the queue holds. A queue nobody drains — a
/// keepalive failing against an idle daemon, or a triage's attempts before the
/// loop reaches its next append — keeps the most recent this many and drops the
/// oldest, so an outage cannot grow the process without bound.
const ATTEMPT_QUEUE_CAP: usize = 64;

/// Failed HTTP attempts awaiting a durable record (docs/transport.md).
pub type UpstreamAttemptQueue = Arc<Mutex<VecDeque<UpstreamAttemptFailure>>>;

/// Construct an empty attempt queue.
pub fn upstream_attempt_queue() -> UpstreamAttemptQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Enqueue one failed attempt, evicting the oldest past [`ATTEMPT_QUEUE_CAP`].
pub fn push_upstream_attempt(queue: &UpstreamAttemptQueue, failure: UpstreamAttemptFailure) {
    let mut queue = queue.lock().unwrap();
    queue.push_back(failure);
    while queue.len() > ATTEMPT_QUEUE_CAP {
        queue.pop_front();
    }
}

/// Take everything queued, oldest first. The loop appends each as a record-only
/// entry, so draining is what makes the record durable.
pub fn drain_upstream_attempts(queue: &UpstreamAttemptQueue) -> Vec<UpstreamAttemptFailure> {
    queue.lock().unwrap().drain(..).collect()
}
