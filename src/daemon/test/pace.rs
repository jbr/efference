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
    shared.resolve_approval(&call.id, Approval::Approve, None);
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

/// Mid-task delivery (docs/control-channel.md): turns submitted while the loop
/// is mid-trajectory sit in the turn inbox until the loop drains them at a juncture. The
/// non-blocking drain must (a) return every non-blank queued turn in submission order,
/// (b) drop blank turns, and (c) balance the `queued` settle-counter for *every* drained
/// turn — so once the trajectory ends `/settle` can settle instead of reading "busy"
/// forever. Without the counter balance the last assertion fails.
#[test]
fn drain_queued_turns_delivers_queued_turns_and_balances_settle() {
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

    let drained = frontend.drain_queued_turns();
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
    assert!(frontend.drain_queued_turns().is_empty());
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

/// Default pace never blocks the juncture (fast path: no hold, zero speed).
#[tokio::test]
async fn pace_at_juncture_is_a_noop_by_default() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    tokio::time::timeout(Duration::from_millis(200), shared.pace_at_juncture())
        .await
        .expect("default pace does not block the juncture");
}

/// A hold blocks the juncture indefinitely; a **release** ends it immediately (the resume trigger).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hold_blocks_the_juncture_until_released() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(None, Some(true)); // hold

    // While held, the juncture does not return.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), shared.pace_at_juncture())
            .await
            .is_err(),
        "a held juncture blocks"
    );

    // A release from another task wakes the waiting juncture.
    let s2 = Arc::clone(&shared);
    let waiter = tokio::spawn(async move { s2.pace_at_juncture().await });
    tokio::time::sleep(Duration::from_millis(20)).await; // let the pace wait arm + await
    shared.set_pace(None, Some(false)); // release
    tokio::time::timeout(Duration::from_millis(500), waiter)
        .await
        .expect("release wakes the held juncture")
        .expect("waiter task joined");
}

/// The operator's submit resumes: it drops the hold, and its queued turn ends the juncture wait.
#[tokio::test]
async fn a_submit_resumes_and_skips_the_juncture() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(None, Some(true));
    assert!(shared.pace_snapshot().1, "held before submit");

    shared.submit_turn(SubmittedTurn::new("go", "sub-test"));
    assert!(!shared.pace_snapshot().1, "submit dropped the hold");

    // A turn is queued, so the juncture returns at once even though we had been held.
    tokio::time::timeout(Duration::from_millis(200), shared.pace_at_juncture())
        .await
        .expect("a queued turn ends the pace wait immediately");
}

/// An agent's turn waits behind the operator's hold (docs/control-channel.md): it is queued and
/// reported held but not sent to the loop, the juncture stays held, and an idle loop holding it
/// reads as settled, since it waits on the operator. The operator's own turn releases the hold,
/// and both reach the loop in submission order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agents_turn_waits_behind_the_operators_hold() {
    let (shared, mut turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(None, Some(true));
    let from_parent =
        SubmittedTurn::new("from the parent", "a1").sourced(Some("session[parent]".into()));
    assert_eq!(
        shared.submit_named_turn(from_parent, None),
        Ok(Landing::Held)
    );
    assert!(
        shared.pace_snapshot().1,
        "an agent does not release the hold"
    );
    assert_eq!(
        shared.queued_turns().len(),
        1,
        "queued, and shown as queued"
    );
    assert!(turns_rx.try_recv().is_err(), "not sent to the loop");
    assert!(
        shared.is_settled(),
        "an idle loop holding it waits on its operator"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), shared.pace_at_juncture())
            .await
            .is_err(),
        "the juncture stays held"
    );

    let landing = shared.submit_named_turn(SubmittedTurn::new("from me", "o1"), None);
    assert_ne!(landing, Ok(Landing::Held));
    assert!(
        !shared.pace_snapshot().1,
        "the operator's turn releases the hold"
    );
    assert_eq!(turns_rx.recv().await.unwrap().submission_id, "a1");
    assert_eq!(turns_rx.recv().await.unwrap().submission_id, "o1");
}

/// `pace` releasing the hold sends the agent turns it held back.
#[tokio::test]
async fn releasing_the_hold_sends_what_it_held_back() {
    let (shared, mut turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(None, Some(true));
    let from_mount =
        SubmittedTurn::new("from claude code", "m1").sourced(Some("mcp[claude-code]".into()));
    assert_eq!(
        shared.submit_named_turn(from_mount, None),
        Ok(Landing::Held)
    );
    shared.set_pace(None, Some(false));
    assert_eq!(turns_rx.try_recv().unwrap().submission_id, "m1");
}

/// A turn queued before the hold landed waits for the release too: the hold outranks queued
/// input at the juncture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hold_outranks_a_turn_queued_before_it() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let from_parent = SubmittedTurn::new("next", "a1").sourced(Some("session[parent]".into()));
    assert_ne!(
        shared.submit_named_turn(from_parent, None),
        Ok(Landing::Held)
    );
    shared.set_pace(None, Some(true));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), shared.pace_at_juncture())
            .await
            .is_err(),
        "a queued turn does not end a hold"
    );
}

/// An agent's answer to a question is delivered, and leaves the operator's hold in place: the
/// loop then waits at its next juncture.
#[tokio::test]
async fn an_agents_answer_leaves_the_hold() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let mut wait = shared.begin_ask("toolu_q", "Proceed?".into(), 1, 1);
    shared.set_pace(None, Some(true));
    let answer = SubmittedTurn::new("yes", "a1").sourced(Some("mcp[claude-code]".into()));
    assert_eq!(
        shared.submit_named_turn(answer, Some("toolu_q")),
        Ok(Landing::Answered)
    );
    assert_eq!(wait.reply().await.unwrap().text, "yes");
    assert!(shared.pace_snapshot().1, "the hold stands");
}

/// A finite paced sleep is interruptible: a submitted message ends the delay early rather than
/// making the operator's steer wait out the remaining pace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submit_interrupts_a_finite_paced_sleep() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_pace(Some(Duration::from_secs(30)), None); // slow, not held

    let s2 = Arc::clone(&shared);
    let waiter = tokio::spawn(async move { s2.pace_at_juncture().await });
    tokio::time::sleep(Duration::from_millis(20)).await; // let the pace wait enter its sleep
    shared.submit_turn(SubmittedTurn::new("urgent", "sub-test")); // fires the pace interrupt

    tokio::time::timeout(Duration::from_millis(500), waiter)
        .await
        .expect("submit ends the 30s paced sleep early")
        .expect("waiter task joined");
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

    shared.interrupt(Default::default()); // no waiter registered — the notify half reaches nobody
    assert!(
        shared.take_interrupt(),
        "the flag survived the dropped wakeup"
    );
    assert!(!shared.take_interrupt(), "one press, one stop");

    // Repeated presses before anyone reads still amount to one interrupt.
    shared.interrupt(Default::default());
    shared.interrupt(Default::default());
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
    assert!(frontend.drain_queued_turns().is_empty());

    shared.submit_turn(SubmittedTurn::new("fresh", "s3"));
    let delivered = frontend.drain_queued_turns();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].text, "fresh");
}

/// A turn ending with another already queued is not a settle: the queued turn runs at once,
/// so no push says the session is waiting (as when an interrupt ends a turn and a message
/// is queued behind it). With nothing queued, the push goes out.
#[test]
fn a_turn_ending_with_a_turn_queued_sends_no_settle_push() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let settle_pushes = |shared: &Session| {
        shared
            .snapshot()
            .iter()
            .filter(|e| {
                matches!(
                    &e.payload,
                    Payload::Telemetry(crate::log::Telemetry::Push(record))
                        if record.kind == crate::log::MomentKind::Settle
                )
            })
            .count()
    };
    shared.set_phase(LoopPhase::Requesting);
    shared.submit_turn(SubmittedTurn::new("next", "s1"));
    shared.going_idle();
    assert_eq!(settle_pushes(&shared), 0, "a queued turn is about to run");

    assert!(shared.claim_turn("s1", true));
    shared.going_idle();
    assert_eq!(
        settle_pushes(&shared),
        1,
        "nothing queued: the session settled"
    );
}
