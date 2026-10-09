//! Tests for `pace.rs`.

use super::*;

/// The `/settle` predicate (docs/permissions.md) tracks the loop's
/// lifecycle: idle → busy on submit (even before dequeue) → busy mid-turn → *settled* at a gate
/// → busy after the gate resolves → settled again when the loop parks.
#[test]
fn settle_predicate_tracks_the_loop_lifecycle() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert!(shared.is_settled(), "a fresh daemon is idle");

    // Submitted but not yet dequeued — busy (the submit-race guard via the queued count).
    shared.submit_turn(SubmittedTurn::new("hi", "sub-test"));
    assert!(
        !shared.is_settled(),
        "a queued turn is busy before the loop sees it"
    );

    // Dequeued, running mid-turn — still busy.
    shared.claim_turn("sub-test", true);
    assert!(!shared.is_settled(), "mid-turn is busy");

    // A gate is a decision point — settled.
    let call = bash_call("npm install");
    let _gate = shared.register_pending(&call, None);
    assert!(
        shared.is_settled(),
        "a pending gate is a settled decision point"
    );

    // Resolving the gate while the turn continues — busy again.
    shared.resolve_approval(&call.id, Approval::Approve);
    assert!(!shared.is_settled(), "post-gate, mid-turn is busy");

    // The loop parks — idle, settled.
    shared.going_idle();
    assert!(
        shared.is_settled(),
        "a parked loop with nothing queued is settled"
    );
}

/// `await_settled` blocks while the loop is busy and returns the moment it parks — and the
/// `enable()`-before-check ordering means a `going_idle()` racing the registration isn't lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn await_settled_returns_when_the_loop_parks() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.submit_turn(SubmittedTurn::new("hi", "sub-test"));
    shared.claim_turn("sub-test", true); // busy, mid-turn

    let waiter = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { shared.await_settled().await })
    };
    // Give the waiter a chance to register and confirm it's still blocked on a busy loop.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !waiter.is_finished(),
        "await_settled must block while the loop is busy"
    );

    // Parking settles it; the waiter wakes and returns.
    shared.going_idle();
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("await_settled should return promptly after going idle")
        .expect("waiter task panicked");
}

/// The seam-drain (docs/control-channel.md): turns submitted while the loop
/// is mid-trajectory sit in the turn inbox until the loop drains them at a juncture. The
/// non-blocking drain must (a) return every non-blank queued turn in submission order,
/// (b) drop blank turns, and (c) balance the `queued` settle-counter for *every* drained
/// turn — so once the trajectory ends `/settle` can settle instead of reading "busy"
/// forever. Without the counter balance the last assertion fails.
#[test]
fn drain_injected_turns_delivers_queued_turns_and_balances_settle() {
    let (shared, turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns: turns_rx,
    };

    // Three turns arrive while the loop is busy (mid-trajectory), one of them blank.
    shared.submit_turn(SubmittedTurn::new("first", "sub-1"));
    shared.submit_turn(SubmittedTurn::new("   ", "sub-2"));
    shared.submit_turn(SubmittedTurn::new("second", "sub-3"));
    assert!(
        !shared.is_settled(),
        "queued turns → not settled while they wait"
    );

    let drained = frontend.drain_injected_turns();
    assert_eq!(
        drained.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        vec!["first", "second"],
        "non-blank turns in submission order; the blank one is dropped"
    );
    assert!(
        shared.is_settled(),
        "every drained turn (blank included) balanced the queued counter, so the loop can settle"
    );

    // A second drain with nothing queued is an empty, no-op pull.
    assert!(frontend.drain_injected_turns().is_empty());
}

/// Pace (docs/control-channel.md) is operator-view state: `set_pace` moves the
/// speed dial and the hold overlay independently, `held` is an overlay that retains `speed`
/// underneath (resume returns to the retained speed, never zero), and `/state` reports both.
#[test]
fn set_pace_moves_the_dials_and_retains_speed_under_a_hold() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    // Default: fast, not held.
    assert_eq!(shared.pace_snapshot(), (Duration::ZERO, false));

    let (speed, held) = shared.set_pace(Some(Duration::from_secs(10)), Some(true));
    assert_eq!((speed, held), (Duration::from_secs(10), true));

    // Releasing keeps the retained speed — the overlay drops, the dial does not zero.
    let (speed, held) = shared.set_pace(None, Some(false));
    assert_eq!((speed, held), (Duration::from_secs(10), false));

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["pace"]["speed_ms"], 10_000);
    assert_eq!(v["pace"]["held"], false);
}

/// Default pace never blocks the seam (fast path: no hold, zero speed).
#[tokio::test]
async fn pace_at_seam_is_a_noop_by_default() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    tokio::time::timeout(Duration::from_millis(200), shared.pace_at_seam())
        .await
        .expect("default pace does not block the seam");
}

/// A hold blocks the seam indefinitely; a **release** ends it immediately (the resume trigger).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hold_blocks_the_seam_until_released() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(None, Some(true)); // hold

    // While held, the seam does not return.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), shared.pace_at_seam())
            .await
            .is_err(),
        "a held seam blocks"
    );

    // A release from another task wakes the waiting seam.
    let s2 = Arc::clone(&shared);
    let seam = tokio::spawn(async move { s2.pace_at_seam().await });
    tokio::time::sleep(Duration::from_millis(20)).await; // let the seam arm + await
    shared.set_pace(None, Some(false)); // release
    tokio::time::timeout(Duration::from_millis(500), seam)
        .await
        .expect("release wakes the held seam")
        .expect("seam task joined");
}

/// Submit ⇒ resume, always: a submitted turn drops the hold, and a queued turn skips the seam
/// wait outright (never pace over pending input).
#[tokio::test]
async fn a_submit_resumes_and_skips_the_seam() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(None, Some(true));
    assert!(shared.pace_snapshot().1, "held before submit");

    shared.submit_turn(SubmittedTurn::new("go", "sub-test"));
    assert!(!shared.pace_snapshot().1, "submit dropped the hold");

    // A turn is queued, so the seam returns at once even though we had been held.
    tokio::time::timeout(Duration::from_millis(200), shared.pace_at_seam())
        .await
        .expect("a queued turn ends the seam wait immediately");
}

/// A finite paced sleep is interruptible: a submitted message ends the delay early rather than
/// making the operator's steer wait out the remaining pace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submit_interrupts_a_finite_paced_sleep() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(Some(Duration::from_secs(30)), None); // slow, not held

    let s2 = Arc::clone(&shared);
    let seam = tokio::spawn(async move { s2.pace_at_seam().await });
    tokio::time::sleep(Duration::from_millis(20)).await; // let the seam enter its sleep
    shared.submit_turn(SubmittedTurn::new("urgent", "sub-test")); // fires the pace interrupt

    tokio::time::timeout(Duration::from_millis(500), seam)
        .await
        .expect("submit ends the 30s paced sleep early")
        .expect("seam task joined");
}

/// `POST /loop/interrupt` **latches** (docs/control-channel.md). The `Notify` half wakes only a
/// currently-registered waiter and stores no permit, so during any tool that doesn't await the
/// abort future — every non-bash backend — the wakeup lands nowhere. The flag is what the loop
/// reads at its junctures instead, and it is a one-shot: consumed exactly once, so a single
/// press stops one call and not the turn after it.
#[test]
fn an_interrupt_latches_and_is_consumed_exactly_once() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert!(!shared.take_interrupt(), "nothing latched at rest");

    shared.interrupt(); // no waiter registered — the notify half reaches nobody
    assert!(
        shared.take_interrupt(),
        "the flag survived the dropped wakeup"
    );
    assert!(!shared.take_interrupt(), "one press, one stop");

    // Repeated presses before anyone reads still amount to one interrupt.
    shared.interrupt();
    shared.interrupt();
    assert!(shared.take_interrupt());
    assert!(!shared.take_interrupt());
}

/// Recall is a **suffix pop**: withdrawing by the id of an older pending turn takes it and
/// everything after it, in submission order, and leaves what came before untouched.
#[test]
fn withdraw_pops_the_suffix_from_the_named_turn() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    for (text, id) in [("one", "s1"), ("two", "s2"), ("three", "s3")] {
        shared.submit_turn(SubmittedTurn::new(text, id));
    }
    let popped = shared.withdraw_turns("s2");
    assert_eq!(
        popped.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        ["two", "three"]
    );
    assert_eq!(
        shared
            .queued_turns()
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>(),
        ["one"]
    );
    // The queue is still non-empty, so the daemon is still busy.
    assert!(!shared.is_settled());
    // Withdrawing the last one settles the daemon: nothing is pending and nothing ran.
    assert_eq!(shared.withdraw_turns("s1").len(), 1);
    assert!(shared.is_settled(), "an emptied queue settles");
}

/// The lost race: the loop claimed the turn before the withdraw landed, so the withdraw pops
/// nothing rather than handing back text the model is already being sent.
#[test]
fn withdraw_pops_nothing_once_the_loop_has_claimed_the_turn() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.submit_turn(SubmittedTurn::new("sent", "s1"));
    assert!(shared.claim_turn("s1", true));
    assert!(shared.withdraw_turns("s1").is_empty());
}

/// A turn withdrawn while already in flight down the channel fails its claim, so the loop drops
/// it unappended — the pending queue, not the channel, is the authority.
#[tokio::test]
async fn a_withdrawn_turn_is_dropped_rather_than_delivered() {
    let (shared, turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns: turns_rx,
    };
    shared.submit_turn(SubmittedTurn::new("recalled", "s1"));
    shared.submit_turn(SubmittedTurn::new("kept", "s2"));
    assert_eq!(
        shared.withdraw_turns("s1").len(),
        2,
        "suffix pop takes both"
    );
    // Both are still sitting in the channel; neither may be delivered.
    assert!(frontend.drain_injected_turns().is_empty());

    shared.submit_turn(SubmittedTurn::new("fresh", "s3"));
    let delivered = frontend.drain_injected_turns();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].text, "fresh");
}
