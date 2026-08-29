//! Tests for `presence.rs`.

use super::*;
use crate::persist::Log;

fn shared() -> Arc<Shared> {
    Shared::new(Log::in_memory()).0
}

/// `?presence=` names one of two states; anything else is rejected rather than read
/// as the default, since a misspelled opt-out would count as a watcher.
#[test]
fn presence_parses_on_off_and_rejects_anything_else() {
    assert_eq!(Presence::parse(None).unwrap(), Presence::On);
    assert_eq!(Presence::parse(Some("on")).unwrap(), Presence::On);
    assert_eq!(Presence::parse(Some("off")).unwrap(), Presence::Off);
    for bad in ["", "false", "0", "Off", "no"] {
        assert!(
            Presence::parse(Some(bad)).is_err(),
            "{bad:?} should not parse"
        );
    }
}

/// The count follows guard lifetime: a guard adds one, dropping it takes it back,
/// and the predicate is nonzero-count.
#[test]
fn a_guard_counts_and_its_drop_uncounts() {
    let shared = shared();
    assert_eq!(shared.watcher_count(), 0);
    assert!(!shared.watched());

    let first = shared.watch(Presence::On);
    assert_eq!(shared.watcher_count(), 1);
    assert!(shared.watched());

    let second = shared.watch(Presence::On);
    assert_eq!(shared.watcher_count(), 2);

    drop(first);
    // Still watched: one connection leaving does not clear the others.
    assert_eq!(shared.watcher_count(), 1);
    assert!(shared.watched());

    drop(second);
    assert_eq!(shared.watcher_count(), 0);
    assert!(!shared.watched());
}

/// An opted-out connection is not a watcher, and its guard leaves the count alone
/// on the way in *and* on the way out — an observer agent connecting and leaving
/// while a human watches must not decrement the human's claim.
#[test]
fn presence_off_is_excluded_from_the_count() {
    let shared = shared();
    let observer = shared.watch(Presence::Off);
    assert_eq!(shared.watcher_count(), 0);
    assert!(!shared.watched());

    let human = shared.watch(Presence::On);
    drop(observer);
    assert_eq!(shared.watcher_count(), 1);
    assert!(shared.watched());
    drop(human);
    assert_eq!(shared.watcher_count(), 0);
}

/// The guard holds the state alive, so a connection outliving every other handle
/// still releases its claim against the same counter rather than a freed one.
#[test]
fn a_guard_outliving_the_callers_handle_still_releases() {
    let shared = shared();
    let guard = shared.watch(Presence::On);
    let weak = Arc::downgrade(&shared);
    drop(shared);
    // The guard's own `Arc` is what is left holding the state.
    assert_eq!(weak.upgrade().map(|s| s.watcher_count()), Some(1));
    drop(guard);
    assert!(weak.upgrade().is_none());
}
