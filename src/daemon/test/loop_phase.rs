//! The loop phase against a real turn loop and a scripted upstream
//! (docs/control-api.md): `settle` returns at the phases that wait on someone outside
//! the loop and at idle, and at no step in between.

use super::upstream_hold::{FIRST_PAUSE, start, start_with};
use super::*;
use crate::daemon::{BlockedReason, LoopPhase};
use crate::frontend::SubmittedTurn;
use crate::record::log::Stance;
use crate::testing::upstream::{Canned, ScriptedUpstream, Turnstile, end_turn, tool_call};

const OVERLOADED: &str = r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#;

/// A `settle` started now: resolves with the `/state` it answered with.
fn settle(session: &Arc<Session>) -> tokio::task::JoinHandle<Value> {
    let session = Arc::clone(session);
    tokio::spawn(async move {
        session.await_settled().await;
        serde_json::from_str(&session.state_json()).unwrap()
    })
}

/// **The 2026-09-12 case.** A `settle` racing a tool's result and its triage stays blocked
/// until the loop is idle: the triage is a request in flight, not a decision point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_waits_through_a_results_triage() {
    // The triage's answer waits for the test, so a `settle` that should block is seen
    // blocking however the threads are scheduled.
    let triage = Turnstile::new();
    let held = triage.clone();
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)).held(&held),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    session
        .wait_until_with(&upstream, "the triage is in flight", || {
            upstream.count() >= 2
        })
        .await;
    assert!(
        session
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::ToolResult { .. })),
        "the result has landed"
    );
    assert_eq!(session.loop_phase(), LoopPhase::Requesting);
    assert!(!waiter.is_finished(), "settle does not answer mid-triage");
    triage.open();

    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    host.shut_down().await;
}

/// **The 2026-10-03 guard.** Under `autonomous`, a call the spotter would hold is
/// declined inside the loop and the model is told so; the decline never enters the
/// gated phase, so a `settle` racing it answers only when the turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_waits_through_an_autonomous_decline() {
    let next = Turnstile::new();
    let held = next.clone();
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        _ => Canned::stream(end_turn()).held(&held),
    });
    let (host, runtime, _dir) = start_with(&upstream, |boot| {
        boot.autonomous = true;
        boot.spotter.stance = Stance::Watch;
        boot.spotter.reason = "watch-for: echo".into();
    })
    .await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    session
        .wait_until_with(
            &upstream,
            "the decline is logged and the next request sent",
            || upstream.count() >= 2,
        )
        .await;
    assert!(
        session.snapshot().iter().any(
            |e| matches!(&e.payload, Payload::ToolResult { call_id, .. } if call_id.0 == "toolu_1")
        ),
        "the declined call has its result"
    );
    assert!(!waiter.is_finished(), "a decline is not a decision point");
    next.open();

    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    assert_eq!(
        session
            .snapshot()
            .iter()
            .filter(|e| matches!(e.payload, Payload::Assistant { .. }))
            .count(),
        2,
        "settle answered after the turn's last completion"
    );
    host.shut_down().await;
}

/// A held pace stops the loop at its juncture until someone releases it: `settle`
/// answers there, naming the phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_returns_on_a_held_pace() {
    let first = Turnstile::new();
    let held = first.clone();
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)).held(&held),
        1 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    // A submit releases a hold, so the hold is set once the turn is under way.
    session
        .wait_until_with(&upstream, "the first request is out", || {
            upstream.count() >= 1
        })
        .await;
    session.set_pace(None, Some(true));
    first.open();
    let state = settle(&session).await.unwrap();
    assert_eq!(
        state["loop_phase"],
        serde_json::json!({"phase": "blocked", "reason": "pace_held"})
    );

    session.set_pace(None, Some(false));
    assert_ne!(
        session.loop_phase(),
        LoopPhase::Blocked {
            reason: BlockedReason::PaceHeld
        },
        "the release ends the hold before the loop wakes"
    );
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    host.shut_down().await;
}

/// A finite pace delay ends on its own timer: `settle` waits through it. The scripted
/// call runs a real process, so the clock is paused once the loop is delayed at its
/// juncture, with nothing left to wait on but the timer (`upstream_hold.rs`). The delay
/// is long enough that the real time between the juncture and the pause cannot reach it.
#[tokio::test]
async fn settle_waits_through_a_pace_delay() {
    const PACE: Duration = Duration::from_secs(600);
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.set_pace(Some(PACE), None);
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    session
        .wait_until("the loop is delayed at its juncture", || {
            matches!(session.loop_phase(), LoopPhase::Delayed { .. })
        })
        .await;
    tokio::time::pause();
    assert!(!session.is_settled());
    assert!(
        !waiter.is_finished(),
        "a timed delay is not a decision point"
    );

    // Half the delay passes, and the loop is still held at its juncture.
    tokio::time::sleep(PACE / 2).await;
    assert!(
        matches!(session.loop_phase(), LoopPhase::Delayed { .. }),
        "{:?}",
        session.loop_phase()
    );
    assert!(!session.is_settled());
    assert!(!waiter.is_finished(), "settle waits out the delay");

    // The rest of it ends on its own, and the turn finishes.
    let state = tokio::time::timeout(PACE, waiter)
        .await
        .expect("the delay ends on its timer")
        .unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    assert_eq!(upstream.count(), 3, "the turn went on after the delay");
    host.shut_down().await;
}

/// The pause between upstream attempts is a delay that ends on a timer: `settle` waits
/// through it, and the operator's continue cuts it short. On a paused clock: the pause
/// is observed before any of it passes, most of it passes with the loop still held, and
/// the continue ends it with the rest unspent.
#[tokio::test(start_paused = true)]
async fn settle_waits_through_a_retry_pause() {
    // The transport makes four attempts before the loop sees the failure.
    let upstream = ScriptedUpstream::new(|n| match n {
        0..4 => Canned::json(503, OVERLOADED),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    session
        .wait_until("the loop pauses between attempts", || {
            matches!(session.loop_phase(), LoopPhase::Delayed { .. })
        })
        .await;
    let paused_at = tokio::time::Instant::now();
    let LoopPhase::Delayed { until } = session.loop_phase() else {
        unreachable!()
    };
    assert_eq!(
        Some(until),
        session.upstream_retry.get().map(|r| r.next_attempt_at),
        "the delay ends when the retry topic says the next attempt fires"
    );
    assert!(!session.is_settled());
    assert!(
        !waiter.is_finished(),
        "a retry pause is not a decision point"
    );

    // Nearly all of the first pause passes with the loop still held.
    tokio::time::sleep(FIRST_PAUSE * 9 / 10).await;
    assert!(
        matches!(session.loop_phase(), LoopPhase::Delayed { .. }),
        "{:?}",
        session.loop_phase()
    );
    assert!(!session.is_settled());
    assert!(!waiter.is_finished(), "settle waits out the pause");

    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue retries now"
    );
    let state = tokio::time::timeout(FIRST_PAUSE, waiter)
        .await
        .expect("continue ends the pause")
        .unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    assert!(
        paused_at.elapsed() < FIRST_PAUSE,
        "the continue ended the pause, not its timer: {:?}",
        paused_at.elapsed()
    );
    host.shut_down().await;
}

/// The config-repair park stops the loop until a human fixes the file: `settle` answers
/// there, naming the phase, and the repair moves the loop on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_returns_on_a_config_repair_park() {
    let upstream = ScriptedUpstream::new(|_| Canned::stream(end_turn()));
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.require_config_repair(ConfigRepair {
        path: "/repo/config.toml".into(),
        at: 0,
        message: "config unparseable".into(),
        parse_error: "bad".into(),
        restore_error: "worse".into(),
    });
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let state = settle(&session).await.unwrap();
    assert_eq!(
        state["loop_phase"],
        serde_json::json!({"phase": "blocked", "reason": "config_repair"})
    );
    assert_eq!(upstream.count(), 0, "nothing is sent while parked");

    session.clear_config_repair();
    session
        .wait_until_with(&upstream, "the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle && upstream.count() == 1
        })
        .await;
    host.shut_down().await;
}

/// An idle triage is a request on the main prefix: it reads `requesting`, `settle` waits
/// through it, and one that pins nothing returns to idle without a turn having ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_triage_reads_requesting_and_settle_waits_through_it() {
    let triage = Turnstile::new();
    let held = triage.clone();
    let upstream = ScriptedUpstream::new(move |_| Canned::stream(end_turn()).held(&held));
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.notify(Notification::new("ci", "the build finished", None));

    session
        .wait_until("the idle triage is in flight", || {
            session.loop_phase() == LoopPhase::Requesting
        })
        .await;
    let waiter = settle(&session);
    session
        .wait_until_with(&upstream, "the idle triage's request is out", || {
            upstream.count() >= 1
        })
        .await;
    assert!(
        !waiter.is_finished(),
        "settle waits through the idle triage"
    );
    triage.open();

    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    assert!(upstream.count() >= 1);
    assert_eq!(session.turns_ended(), 0, "no turn ran");
    host.shut_down().await;
}
