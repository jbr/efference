//! The wait the tests share (docs/control-channel.md): a predicate is read again when the
//! session, or the host, signals that something it could read has moved, and one hang
//! guard bounds a wait that can never succeed.

use super::*;
use std::future::Future;
use std::task::Poll;
use tokio::sync::futures::Notified;

/// How long a wait may go without its predicate holding before the test fails. A guard
/// against a wait that cannot succeed, not a bound on how long the awaited event may take:
/// a wait that succeeds returns at the wake that makes its predicate true, however late.
const HANG_GUARD: Duration = Duration::from_secs(60);

/// How often [`Session::poll_until`] and [`Host::poll_until`] read their predicate when no
/// signal has woken it.
pub(super) const POLL: Duration = Duration::from_millis(20);

/// The log entries a wait that gave up prints.
const TAIL: usize = 8;

/// The characters of one log entry's payload a wait that gave up prints.
const BRIEF: usize = 160;

impl Session {
    /// Wait until `ready` holds. It is read again at each signal the session raises that a
    /// state reader wakes on (`watch_topics`, `topics.rs`): a phase or gate change, a state
    /// ping, a log append, and a change to the retry cell. A predicate that reads anything
    /// else — a counter another thread keeps, a file, a process — is not woken when that
    /// moves; that is [`poll_until`](Self::poll_until)'s.
    ///
    /// The signals are armed before the predicate is read, as [`Session::await_settled`]
    /// arms its waiter, so a change landing between the read and the await wakes the wait.
    /// On a hang the test fails with the session's phase, queue, held gate and log tail.
    pub(crate) async fn wait_until(&self, what: &str, ready: impl FnMut() -> bool) {
        until(what, || self.signals(), None, ready, || self.stalled()).await;
    }

    /// [`wait_until`](Self::wait_until) for a predicate that also reads something the
    /// session raises no signal for, such as the requests a mock upstream has counted:
    /// read again every [`POLL`] as well as at each signal.
    pub(crate) async fn poll_until(&self, what: &str, ready: impl FnMut() -> bool) {
        until(
            what,
            || self.signals(),
            Some(POLL),
            ready,
            || self.stalled(),
        )
        .await;
    }

    /// The signals a state reader wakes on, each created now and so armed by [`until`].
    fn signals(&self) -> Vec<Notified<'_>> {
        vec![
            self.settle_changed.notified(),
            self.state_changed.notified(),
            self.appended.notified(),
            self.upstream_retry.changed(),
        ]
    }

    /// What a wait that gave up prints about the session.
    fn stalled(&self) -> String {
        let queue: Vec<String> = self
            .queue
            .lock()
            .unwrap()
            .iter()
            .map(|turn| turn.text.clone())
            .collect();
        let gate = self
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| format!("{} ({}) {}", pending.name, pending.call_id, pending.input));
        let entries = self.snapshot();
        let tail: Vec<String> = entries[entries.len().saturating_sub(TAIL)..]
            .iter()
            .map(|entry| {
                format!(
                    "  [{}] turn {} {}",
                    entry.seq,
                    entry.turn,
                    brief(&format!("{:?}", entry.payload))
                )
            })
            .collect();
        format!(
            "phase: {:?}\nqueue: {queue:?}\npending gate: {gate:?}\nlog: {} entries, the last {}:\n{}",
            self.loop_phase(),
            entries.len(),
            tail.len(),
            tail.join("\n")
        )
    }
}

/// `text`, cut to [`BRIEF`] characters.
fn brief(text: &str) -> String {
    let mut shown: String = text.chars().take(BRIEF).collect();
    if text.chars().nth(BRIEF).is_some() {
        shown.push('…');
    }
    shown
}

/// Read `ready` until it holds, at each of `signals` and, with `poll`, on that interval
/// too; `describe` says where the thing waited on stood, when the hang guard fails the
/// test.
pub(super) async fn until<'a>(
    what: &str,
    signals: impl Fn() -> Vec<Notified<'a>>,
    poll: Option<Duration>,
    mut ready: impl FnMut() -> bool,
    describe: impl Fn() -> String,
) {
    let waiting = async {
        loop {
            // Registered on the waiters before the predicate is read: `notify_waiters`
            // stores no permit, so a signal fired between the read and the await would
            // otherwise be lost.
            let mut armed: Vec<Pin<Box<Notified<'a>>>> =
                signals().into_iter().map(Box::pin).collect();
            for signal in &mut armed {
                signal.as_mut().enable();
            }
            if ready() {
                return;
            }
            let signalled = std::future::poll_fn(|cx| {
                match armed
                    .iter_mut()
                    .any(|signal| signal.as_mut().poll(cx).is_ready())
                {
                    true => Poll::Ready(()),
                    false => Poll::Pending,
                }
            });
            match poll {
                Some(every) => {
                    let _ = tokio::time::timeout(every, signalled).await;
                }
                None => signalled.await,
            }
        }
    };
    if tokio::time::timeout(HANG_GUARD, waiting).await.is_err() {
        panic!(
            "timed out after {}s waiting: {what}\n{}",
            HANG_GUARD.as_secs(),
            describe()
        );
    }
}
