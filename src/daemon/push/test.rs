//! Tests for `push.rs`'s suppression stash.
//!
//! The send itself is not observable from here — `spawn_send` returns without a
//! root secret or a runtime, which is what every test context looks like — so
//! what is asserted is the stash: when a message lands in it, and when the reap
//! of the last watcher takes it back out.

use super::*;
use crate::persist::Log;
use std::time::{Duration, Instant};

fn shared() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

fn stash_of(shared: &Session) -> Option<String> {
    shared
        .suppressed
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.message.title.clone())
}

/// Unwatched, a push goes straight to the send path and nothing is stashed.
#[test]
fn an_unwatched_push_is_not_stashed() {
    let shared = shared();
    shared.push("Gate", "body".into());
    assert_eq!(stash_of(&shared), None);
}

/// Watched, a push is stashed instead of sent, and a later one supersedes it.
#[test]
fn a_watched_push_is_stashed_and_the_latest_wins() {
    let shared = shared();
    let _guard = shared.watch(Presence::On);
    shared.push("Gate", "first".into());
    shared.push("Settle", "second".into());
    assert_eq!(stash_of(&shared), Some("Settle".into()));
}

/// The reap of the last watcher takes the stash — the guard's drop is what
/// discovers that the suppressing connection may already have been dead.
#[test]
fn reaping_the_last_watcher_takes_the_stash() {
    let shared = shared();
    let first = shared.watch(Presence::On);
    let second = shared.watch(Presence::On);
    shared.push("Gate", "body".into());

    drop(first);
    // One watcher remains, so the suppression stands.
    assert_eq!(stash_of(&shared), Some("Gate".into()));

    drop(second);
    assert_eq!(stash_of(&shared), None);
}

/// The window arithmetic: a recent suppression is replayable, a stale one is
/// taken but not sent — the connection that suppressed it outlived the daemon's
/// blindness to a close, so a person watched the event happen.
#[test]
fn only_a_recent_suppression_is_replayable() {
    let shared = shared();
    let message = crate::push::Message::new("Gate", "body");
    *shared.suppressed.lock().unwrap() = Some(Suppressed {
        at: Instant::now(),
        message: message.clone(),
    });
    assert!(shared.take_replayable().is_some());
    assert!(shared.take_replayable().is_none(), "take empties the stash");

    *shared.suppressed.lock().unwrap() = Some(Suppressed {
        at: Instant::now() - (REPLAY_WINDOW + Duration::from_secs(1)),
        message,
    });
    assert!(shared.take_replayable().is_none());
    assert_eq!(stash_of(&shared), None, "a stale stash is still consumed");
}

/// An opted-out connection leaving does not touch the stash: it never counted,
/// so its departure says nothing about who was watching.
#[test]
fn an_observers_departure_does_not_replay() {
    let shared = shared();
    let _human = shared.watch(Presence::On);
    let observer = shared.watch(Presence::Off);
    shared.push("Gate", "body".into());
    drop(observer);
    assert_eq!(stash_of(&shared), Some("Gate".into()));
}
