//! The loop phase against a real turn loop and a scripted upstream
//! (docs/control-api.md): `settle` returns at the phases that wait on someone outside
//! the loop and at idle, and at no step in between.

use super::upstream_hold::{Answer, end_turn, scripted, start, start_with, tool_call, wait_until};
use super::*;
use crate::daemon::{BlockedReason, LoopPhase};
use crate::frontend::SubmittedTurn;
use crate::log::Stance;
use std::sync::atomic::AtomicUsize;

const OVERLOADED: &str = r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#;

/// A `settle` started now: resolves with the `/state` it answered with.
fn settle(session: &Arc<Session>) -> tokio::task::JoinHandle<Value> {
    let session = Arc::clone(session);
    tokio::spawn(async move {
        session.await_settled().await;
        serde_json::from_str(&session.state_json()).unwrap()
    })
}

/// Holds a scripted answer until the test opens it, so a `settle` that should block is
/// seen blocking however the threads are scheduled. One static per test: the scripts
/// are plain `fn`s.
pub(super) struct Latch(std::sync::atomic::AtomicBool);

impl Latch {
    pub(super) const fn new() -> Self {
        Self(std::sync::atomic::AtomicBool::new(false))
    }

    pub(super) fn wait(&self) {
        let start = std::time::Instant::now();
        while !self.0.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub(super) fn open(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// **The 2026-09-12 case.** A `settle` racing a tool's result and its triage stays blocked
/// until the loop is idle: the triage is a request in flight, not a decision point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_waits_through_a_results_triage() {
    static TRIAGE: Latch = Latch::new();
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => {
            TRIAGE.wait();
            Answer::Stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#))
        }
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    wait_until("the triage is in flight", || {
        requests.load(Ordering::SeqCst) >= 2
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
    TRIAGE.open();

    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    host.shut_down().await;
}

/// **The 2026-10-03 guard.** Under `autonomous`, a call the spotter would hold is
/// declined inside the loop and the model is told so; the decline never enters the
/// gated phase, so a `settle` racing it answers only when the turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_waits_through_an_autonomous_decline() {
    static NEXT: Latch = Latch::new();
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        _ => {
            NEXT.wait();
            Answer::Stream(end_turn())
        }
    });
    let (host, runtime) = start_with(addr, |boot| {
        boot.autonomous = true;
        boot.spotter.stance = Stance::Watch;
        boot.spotter.reason = "watch-for: echo".into();
    });
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    wait_until("the decline is logged and the next request sent", || {
        requests.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(
        session.snapshot().iter().any(
            |e| matches!(&e.payload, Payload::ToolResult { call_id, .. } if call_id.0 == "toolu_1")
        ),
        "the declined call has its result"
    );
    assert!(!waiter.is_finished(), "a decline is not a decision point");
    NEXT.open();

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
    static FIRST: Latch = Latch::new();
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0 => {
            FIRST.wait();
            Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#))
        }
        1 => Answer::Stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    // A submit releases a hold, so the hold is set once the turn is under way.
    wait_until("the first request is out", || {
        requests.load(Ordering::SeqCst) >= 1
    })
    .await;
    session.set_pace(None, Some(true));
    FIRST.open();
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
    wait_until("the turn finishes", || {
        session.loop_phase() == LoopPhase::Idle
    })
    .await;
    host.shut_down().await;
}

/// A finite pace delay ends on its own timer: `settle` waits through it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_waits_through_a_pace_delay() {
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Answer::Stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.set_pace(Some(Duration::from_secs(2)), None);
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    wait_until("the loop is delayed at its juncture", || {
        matches!(session.loop_phase(), LoopPhase::Delayed { .. })
    })
    .await;
    assert!(!session.is_settled());
    assert!(
        !waiter.is_finished(),
        "a timed delay is not a decision point"
    );
    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    host.shut_down().await;
}

/// The pause between upstream attempts is a delay that ends on a timer: `settle` waits
/// through it, and the operator's continue cuts it short.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_waits_through_a_retry_pause() {
    let requests = Arc::new(AtomicUsize::new(0));
    // The transport makes four attempts before the loop sees the failure.
    let addr = scripted(Arc::clone(&requests), |n| match n {
        0..4 => Answer::Status(503, OVERLOADED),
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    let waiter = settle(&session);

    wait_until("the loop pauses between attempts", || {
        matches!(session.loop_phase(), LoopPhase::Delayed { .. })
    })
    .await;
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
    assert_eq!(session.request_continue(), Ok(()), "continue retries now");
    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    host.shut_down().await;
}

/// The config-repair park stops the loop until a human fixes the file: `settle` answers
/// there, naming the phase, and the repair moves the loop on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_returns_on_a_config_repair_park() {
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |_| Answer::Stream(end_turn()));
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.require_config_repair(crate::daemon::ConfigRepair {
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
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "nothing is sent while parked"
    );

    session.clear_config_repair();
    wait_until("the turn finishes", || {
        session.loop_phase() == LoopPhase::Idle && requests.load(Ordering::SeqCst) == 1
    })
    .await;
    host.shut_down().await;
}

/// An idle triage is a request on the main prefix: it reads `requesting`, `settle` waits
/// through it, and one that pins nothing returns to idle without a turn having ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_triage_reads_requesting_and_settle_waits_through_it() {
    static TRIAGE: Latch = Latch::new();
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |_| {
        TRIAGE.wait();
        Answer::Stream(end_turn())
    });
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.notify("ci".into(), "the build finished".into(), None);

    wait_until("the idle triage is in flight", || {
        session.loop_phase() == LoopPhase::Requesting
    })
    .await;
    let waiter = settle(&session);
    wait_until("the idle triage's request is out", || {
        requests.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(
        !waiter.is_finished(),
        "settle waits through the idle triage"
    );
    TRIAGE.open();

    let state = waiter.await.unwrap();
    assert_eq!(state["loop_phase"], serde_json::json!({"phase": "idle"}));
    assert!(requests.load(Ordering::SeqCst) >= 1);
    assert_eq!(session.turns_ended(), 0, "no turn ran");
    host.shut_down().await;
}
