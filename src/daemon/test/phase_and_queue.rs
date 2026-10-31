//! Tests for `Session::phase_and_queue` (docs/control-channel.md): a writer that hands the
//! loop its input, landing between its read of the phase and its read of the queue, is seen
//! whole or not at all — so `settle`, a turn's delivery, and a continue's verdict never read
//! "idle, nothing queued" at an instant it did not hold — and a turn the hold holds back is
//! never seen queued before it is held back.

use super::*;
use crate::daemon::continuation::NotContinuable;
use std::sync::atomic::AtomicBool;

/// Arm the next `phase_and_queue` to run `writer` between its read of the phase and its read
/// of the queue, where a writer on another thread lands if no lock it needs is held there.
/// Every writer here takes the queue lock, so it runs only if the queue lock is free; held,
/// the lock shuts the writer out, and a real one would wait for the reads to end. Returns
/// whether the point between the reads was reached.
fn between_reads(
    session: &Session,
    writer: impl FnOnce(&Session) + Send + 'static,
) -> Arc<AtomicBool> {
    let reached = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&reached);
    *session.between_reads.lock().unwrap() = Some(Box::new(move |session: &Session| {
        flag.store(true, Ordering::SeqCst);
        let queue_free = session.queue.try_lock().is_ok();
        if queue_free {
            writer(session);
        }
    }));
    reached
}

/// An idle loop with a submitted turn it has not claimed yet.
fn idle_with_a_turn_queued() -> Arc<Session> {
    let (session, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    session.append(1, user("hello")).unwrap();
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    session
}

/// **The settle race.** The idle loop claims the queued turn while a `settle` is between its
/// reads: `settle` sees the turn queued or the loop running it, not an idle loop with an
/// empty queue.
#[test]
fn a_claim_between_the_reads_is_not_settled() {
    let session = idle_with_a_turn_queued();
    let reached = between_reads(&session, |s| {
        s.claim_turn("s1", true);
    });
    let settled = session.is_settled();
    assert!(
        reached.load(Ordering::SeqCst),
        "the point between the reads was reached"
    );
    assert!(!settled, "a turn being claimed is not a decision point");
}

/// **The mirror.** Mid-turn, a turn is submitted and the turn ends while a `settle` is between
/// its reads: `settle` does not pair the empty queue from before the submit with the idle
/// phase after the turn ended, as reading the queue first and the phase after would.
#[test]
fn a_submit_and_a_turn_ending_between_the_reads_are_not_settled() {
    let session = idle_with_a_turn_queued();
    session.claim_turn("s1", true);
    let reached = between_reads(&session, |s| {
        s.submit_turn(SubmittedTurn::new("next", "s2"));
        s.going_idle();
    });
    let settled = session.is_settled();
    assert!(
        reached.load(Ordering::SeqCst),
        "the point between the reads was reached"
    );
    assert!(
        !settled,
        "a turn ending with another queued is not a decision point"
    );
}

/// The loop takes a pending continue while a `settle` is between its reads: `settle` sees the
/// continue pending or taken, not an idle loop with nothing pending.
#[test]
fn a_continue_taken_between_the_reads_is_not_settled() {
    let (session, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    session.append(1, user("hello")).unwrap();
    session.request_continue(Default::default()).unwrap();
    let reached = between_reads(&session, Session::take_continue);
    let settled = session.is_settled();
    assert!(
        reached.load(Ordering::SeqCst),
        "the point between the reads was reached"
    );
    assert!(!settled, "a continue being taken is not a decision point");
}

/// A turn submitted while the loop claims the one before it waits behind that turn, so the
/// `turn` verb says `queued`.
#[test]
fn a_claim_between_the_reads_leaves_the_next_turn_queued() {
    let session = idle_with_a_turn_queued();
    let reached = between_reads(&session, |s| {
        s.claim_turn("s1", true);
    });
    let delivery = session.turn_delivery();
    assert!(
        reached.load(Ordering::SeqCst),
        "the point between the reads was reached"
    );
    assert_eq!(delivery, "queued");
}

/// A continue asked for while the loop claims a turn is declined as busy: accepted, it would
/// re-prompt the wire once the claimed turn ended.
#[test]
fn a_claim_between_the_reads_declines_a_continue() {
    let session = idle_with_a_turn_queued();
    let reached = between_reads(&session, |s| {
        s.claim_turn("s1", true);
    });
    let verdict = session.continue_verdict();
    assert!(
        reached.load(Ordering::SeqCst),
        "the point between the reads was reached"
    );
    assert_eq!(verdict, Err(NotContinuable::Busy));
}

/// An agent's turn submitted to a held, idle session is queued and held back in one change.
/// A `settle` landing between the two pushes would read it as a turn about to run, answer
/// busy, and wait for a wake that holding it back never sends. `phase_and_queue` takes the
/// pace lock and then the queue lock, so it lands there only if both are free.
#[test]
fn an_agents_turn_is_held_back_as_it_is_queued() {
    let (session, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    session.set_pace(None, Some(true));
    let seen = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&seen);
    *session.between_pushes.lock().unwrap() = Some(Box::new(move |session: &Session| {
        let free = session.pace.try_lock().is_ok() && session.queue.try_lock().is_ok();
        let turn_to_run = free.then(|| session.phase_and_queue().turn_to_run);
        *slot.lock().unwrap() = Some(turn_to_run);
    }));
    let from_parent =
        SubmittedTurn::new("from the parent", "a1").sourced(Some("session[parent]".into()));
    assert_eq!(
        session.submit_named_turn(from_parent, None),
        Ok(Landing::Held)
    );
    let seen = seen.lock().unwrap().take();
    assert!(seen.is_some(), "the point between the pushes was reached");
    assert_ne!(
        seen,
        Some(Some(true)),
        "a read between the pushes saw the held-back turn as one to run"
    );
    assert!(
        session.is_settled(),
        "an idle loop holding it waits on its operator"
    );
}
