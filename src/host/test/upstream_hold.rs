//! Upstream failures against a real turn loop and a scripted upstream
//! (docs/transport.md, docs/triage.md): a failed request takes the retry
//! schedule instead of stalling at once, a triage without a verdict holds
//! its results rather than showing the agent an error, and the operator's
//! continue retries either wait now.
//!
//! The fixtures here are what a live-loop test builds on, and a test whose subject is
//! time builds on them with a paused clock. `#[tokio::test(start_paused = true)]` does
//! for a script that answers without running a call: the clock moves only when the
//! runtime has nothing else to do, and the scripted upstream answers in process. A
//! script that calls `bash` runs a real process, which the runtime also waits on with
//! nothing else to do, so a paused clock races past the call's own deadlines (and the
//! wait helper's hang guard) while the process is still starting. A test with such a
//! script runs on the real clock until the call's result is in and calls
//! `tokio::time::pause()` there.

use super::runtime::{boot, started, workdir};
use super::*;
use crate::host::runtime::SessionRuntime;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream, end_turn, tool_call};
use crate::protocol::state::StallReason;
use crate::record::log::{Origin, Provider, WireState};
use crate::session::submitted::SubmittedTurn;
use tempfile::TempDir;
use trillium_tokio::Swansong;

/// A live loop against `upstream`, in a working directory of its own, returned once it
/// has gone idle (`runtime::started`); the directory goes when the returned guard drops.
pub(super) async fn start(upstream: &ScriptedUpstream) -> (Swansong, SessionRuntime, TempDir) {
    start_with(upstream, |_| {}).await
}

/// [`start`], with the boot adjusted first: a review level, a spotter's stance.
pub(super) async fn start_with(
    upstream: &ScriptedUpstream,
    adjust: impl FnOnce(&mut SessionBoot<crate::curation::spotter::StaticSpotter>),
) -> (Swansong, SessionRuntime, TempDir) {
    let (cwd, dir) = workdir("upstream");
    let mut boot = boot(&cwd);
    boot.client = Client::new(
        upstream.endpoint(Provider::Anthropic, "/v1/messages"),
        "m",
        "sys",
    );
    adjust(&mut boot);
    let host = Swansong::new();
    let runtime = started(boot, &host).await;
    (host, runtime, dir)
}

/// How long the loop waits after the first failed attempt (`stream_abort_backoff`,
/// `agent.rs`), held as a number so a test that waits it out fails when the schedule
/// changes under it.
pub(super) const FIRST_PAUSE: Duration = Duration::from_secs(1);

const REFUSED: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"no"}}"#;
const OVERLOADED: &str = r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#;
/// A failure the loop retries and the transport does not: 408 is outside
/// `RETRY_STATUSES`, so one scripted answer is one upstream request, and a body naming
/// no terminal condition classifies transient (`api/failure.rs`).
const TIMED_OUT: &str = r#"{"error":"timed out"}"#;

/// **A triage without a verdict holds; continue retries it.** The triage's
/// request is refused, so the triage has no verdict. Nothing is logged for the
/// call — no error result the agent would read — and the session reports a stall
/// while its loop still runs. The operator's continue runs the triage again, the
/// verdict is recorded, and the held attempt's breadcrumb is in the `Triage`.
// A scenario read top to bottom: the scripted refusal, the hold, the continue, and the log it
// leaves.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_without_a_verdict_holds_until_continue() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::json(400, REFUSED),
        2 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    let held = LoopPhase::Blocked {
        reason: BlockedReason::TriageUnavailable,
    };
    session
        .wait_until("the triage is held", || session.loop_phase() == held)
        .await;
    let stall = session.stall().unwrap();
    assert_eq!(stall.reason, StallReason::TriageUnavailable);
    assert!(session.is_settled(), "a held triage is a decision point");
    // The result is durable from the moment it existed, but undecided: the entry is
    // there to read and projects to nothing, so a request built before the triage's
    // verdict could not carry half a batch.
    let captured = session
        .snapshot()
        .into_iter()
        .find_map(|e| match e.payload {
            Payload::ToolResult { raw, wire, .. } => Some((raw, wire)),
            _ => None,
        })
        .expect("the captured result is on the log while the triage holds");
    assert!(
        captured.0.content.contains("hi"),
        "the output itself survives the hold: {:?}",
        captured.0.content
    );
    assert!(
        matches!(captured.1, WireState::Pending),
        "and nothing has decided its wire form yet"
    );
    assert!(
        !session
            .project_messages()
            .iter()
            .flat_map(|m| m.content.iter())
            .any(|block| matches!(block, crate::record::project::ApiBlock::ToolResult { .. })),
        "nothing about the held call reaches the main wire"
    );

    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue retries the hold"
    );
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert!(
        session.stall().is_none(),
        "the recovered hold is not a stall"
    );
    let log = session.snapshot();
    let captured_seq = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { .. } => Some(e.seq),
            _ => None,
        })
        .expect("the captured result is in the log");
    assert!(
        log.iter().any(|e| matches!(
            &e.payload,
            Payload::ResultVerdict { for_result, wire: WireState::Triaged(_), .. }
                if *for_result == captured_seq
        )),
        "the verdict names the result it was decided for"
    );
    assert!(
        !log.iter().any(|e| matches!(
            &e.payload,
            Payload::ResultVerdict {
                wire: WireState::TriageFailed { .. },
                ..
            }
        )) && !log.iter().any(|e| matches!(
            &e.payload,
            Payload::ToolResult {
                wire: WireState::TriageFailed { .. },
                ..
            }
        )),
        "no triage failure is ever written"
    );
    let triage = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::Triage(t) if t.for_result.is_some() => Some(t),
            _ => None,
        })
        .expect("the triage");
    assert_eq!(triage.origin, Origin::Model);
    assert!(
        triage.retries.iter().any(|r| r.starts_with("held: ")),
        "the hold is in the record: {:?}",
        triage.retries
    );
    host.shut_down().await;
}

/// **A compaction written during a hold is what the retried triage forks.** The triage holds,
/// a compaction's mark lands (as `compact_once` appends it), and the operator retries: the
/// retried triage carries the account, not the history it replaced. The triage's first
/// snapshot predates the mark, so a retry that reused it would still read the whole
/// unfolded prefix, which is the cold read the compaction exists to avoid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fold_during_a_hold_is_what_the_retry_triages() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::json(400, REFUSED),
        2 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("zebra history to compact away", "s1"));

    session
        .wait_until("the triage is held", || session.stall().is_some())
        .await;
    let upto = session
        .snapshot()
        .iter()
        .find_map(|e| matches!(e.payload, Payload::User(_)).then_some(e.seq))
        .expect("the user turn is logged");
    session
        .append(
            1,
            Payload::Summary(crate::record::log::Summary::consolidation(
                0,
                upto,
                "the compacted account",
            )),
        )
        .unwrap();
    assert_eq!(session.request_continue(Default::default()), Ok(()));
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;

    let requests = upstream.requests();
    let held = &requests[1].body;
    let retried = &requests[2].body;
    assert!(
        held.contains("zebra history"),
        "the held triage read the history"
    );
    assert!(
        retried.contains("the compacted account") && !retried.contains("zebra history"),
        "the retried triage reads the compaction: {retried}"
    );
    host.shut_down().await;
}

/// **A failed request takes the schedule.** The transport exhausts its own
/// attempts on an overloaded upstream and returns an error; the loop records it
/// and waits to retry instead of stalling, and the operator's continue cuts the
/// wait short. On a paused clock the wait would end by itself when the runtime has
/// nothing else to do, so the test lets most of the first pause pass with the loop
/// still waiting, and holds the continue to the rest of it: the turn finishes with
/// that pause unspent.
#[tokio::test(start_paused = true)]
async fn a_failed_request_is_retried_and_continue_skips_the_wait() {
    // The transport makes four attempts before the loop receives the failure.
    let upstream = ScriptedUpstream::new(|n| match n {
        0..4 => Canned::json(503, OVERLOADED),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until("the loop is waiting to retry", || {
            session.upstream_retry_for_test().get().is_some()
        })
        .await;
    let waiting_since = tokio::time::Instant::now();
    assert!(
        session.stall().is_none(),
        "a transient failure does not stall"
    );
    // Most of the first pause passes with the loop still waiting.
    tokio::time::sleep(FIRST_PAUSE * 9 / 10).await;
    assert!(
        session.upstream_retry_for_test().get().is_some(),
        "the loop waits out the pause"
    );
    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue retries now"
    );
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert!(
        waiting_since.elapsed() < FIRST_PAUSE,
        "the continue ended the wait, not its timer: {:?}",
        waiting_since.elapsed()
    );
    assert!(session.stall().is_none());
    assert!(session.upstream_retry_for_test().get().is_none());
    assert!(
        session.snapshot().iter().any(|e| matches!(
            &e.payload,
            Payload::Assistant {
                excluded: Some(_),
                ..
            }
        )),
        "the failed request is in the record"
    );
    host.shut_down().await;
}

/// **Fresh forks, then a harness defect.** A triage that answers every attempt of
/// every fork without a usable verdict is not an upstream condition: after
/// `TRIAGE_RESTARTS` fresh forks of `TRIAGE_ATTEMPTS` each, the triage holds under
/// `TriageDefect`, and still nothing is sent to the agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_triage_that_never_answers_usably_is_held_as_a_defect() {
    use crate::curation::triage::TRIAGE_RESTARTS;
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        // Prose, not a verdict: unusable every time.
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until("the triage is held", || session.stall().is_some())
        .await;
    assert_eq!(session.stall().unwrap().reason, StallReason::TriageDefect);
    // The tool call, then three attempts in each fork.
    assert_eq!(upstream.count(), 1 + 3 * TRIAGE_RESTARTS);
    // Every fork is spent and the result is still undecided: durable in the log, since
    // it was captured before the triage opened, and absent from the wire, since no fork
    // answered for it.
    let captured = session
        .snapshot()
        .into_iter()
        .find_map(|e| match e.payload {
            Payload::ToolResult { raw, wire, .. } => Some((raw, wire)),
            _ => None,
        })
        .expect("the captured result is on the log while the triage holds");
    assert!(
        captured.0.content.contains("hi"),
        "the output itself survives the hold: {:?}",
        captured.0.content
    );
    assert!(matches!(captured.1, WireState::Pending));
    assert!(
        !session
            .project_messages()
            .iter()
            .flat_map(|m| m.content.iter())
            .any(|block| matches!(block, crate::record::project::ApiBlock::ToolResult { .. })),
        "nothing about the held call reaches the main wire"
    );
    host.shut_down().await;
}

/// **An interrupt during the retry wait ends the retry sequence.** The press lands while
/// the loop waits out the first pause: the wait ends at once, the retry state clears, the
/// loop is idle before the pause could have elapsed, and the attempt the wait was holding
/// never goes out — not then, and not after the clock is given the time the wait would
/// have taken. Continue afterwards starts a fresh request on a full budget: its failure
/// is attempt 1 of 12, so the schedule's first delay, one second, is what comes next.
#[tokio::test(start_paused = true)]
async fn an_interrupt_during_the_retry_wait_ends_the_sequence() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0..2 => Canned::json(408, TIMED_OUT),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until("the loop is waiting to retry", || {
            session.upstream_retry_for_test().get().is_some()
        })
        .await;
    assert_eq!(upstream.count(), 1, "one attempt has failed");
    let pressed = tokio::time::Instant::now();
    session.interrupt(Default::default());
    session
        .wait_until("the loop is idle", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert!(
        pressed.elapsed() < FIRST_PAUSE,
        "the press ended the wait, not its timer: {:?}",
        pressed.elapsed()
    );
    assert!(
        session.upstream_retry_for_test().get().is_none(),
        "the retry status clears with the sequence"
    );
    assert!(!session.interrupt_pending(), "the press was consumed");
    // Room for the attempt the wait was holding, and then some.
    tokio::time::sleep(FIRST_PAUSE * 30).await;
    assert_eq!(upstream.count(), 1, "no request after the interrupted wait");

    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue starts a fresh request"
    );
    session
        .wait_until("the fresh request fails and is scheduled", || {
            session
                .upstream_retry_for_test()
                .get()
                .is_some_and(|retry| retry.attempt == 1)
        })
        .await;
    let retry = session.upstream_retry_for_test().get().unwrap();
    assert_eq!(
        retry.budget,
        crate::inference::api::STREAM_ABORT_ATTEMPTS,
        "a full retry budget"
    );
    let now = crate::record::log::Timestamp::now().0;
    assert!(
        retry.next_attempt_at <= now + 1000,
        "the first delay is the schedule's one second, not a later doubling: {}ms",
        retry.next_attempt_at.saturating_sub(now)
    );
    host.shut_down().await;
}

/// **An interrupt during the triage's retry wait withholds the result.** The triage's
/// first attempt fails transiently, so the triage is waiting between attempts when the
/// press lands: the wait ends at once, no further attempt goes out, the retry state
/// clears, and the result is withheld under the same interrupted verdict a press during
/// the triage's request produces — the loop consumes the latch after the gate and ends
/// the turn. Continue afterwards starts a fresh main request; the tool does not run again.
#[tokio::test]
async fn an_interrupt_during_the_triage_retry_wait_withholds_the_result() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::json(408, TIMED_OUT),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until("the triage is waiting to retry", || {
            session
                .upstream_retry_for_test()
                .get()
                .is_some_and(|retry| retry.surface == crate::inference::api::RetrySurface::Triage)
        })
        .await;
    assert_eq!(
        upstream.count(),
        2,
        "the call ran and the triage's first attempt failed"
    );
    // The call's result is in, so the clock can be paused for what follows
    // (`upstream_hold.rs`): the wait's timer and the room after it are the test's to
    // control from here.
    tokio::time::pause();
    let pressed = tokio::time::Instant::now();
    session.interrupt(Default::default());
    session
        .wait_until("the loop is idle", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert!(
        pressed.elapsed() < FIRST_PAUSE,
        "the press ended the wait, not its timer: {:?}",
        pressed.elapsed()
    );
    assert!(
        session.upstream_retry_for_test().get().is_none(),
        "the retry status clears with the sequence"
    );
    assert!(
        !session.interrupt_pending(),
        "the loop consumed the press after the triage"
    );
    tokio::time::sleep(FIRST_PAUSE * 30).await;
    assert_eq!(upstream.count(), 2, "no attempt after the interrupted wait");

    let log = session.snapshot();
    let captured_seq = log
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { .. } => Some(e.seq),
            _ => None,
        })
        .expect("the ran call's result is in the log");
    assert!(
        log.iter().any(|e| matches!(
            &e.payload,
            Payload::ResultVerdict {
                for_result,
                wire: WireState::Interrupted(_),
            } if *for_result == captured_seq
        )),
        "the interrupted triage withholds the result it captured"
    );

    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue starts a fresh request"
    );
    session
        .wait_until_with(&upstream, "the fresh request answered", || {
            upstream.count() == 3
        })
        .await;
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert_eq!(
        session
            .snapshot()
            .iter()
            .filter(|e| matches!(e.payload, Payload::ToolResult { .. }))
            .count(),
        1,
        "the tool did not run again"
    );
    host.shut_down().await;
}

/// **Continue during a backoff skips the wait and keeps the attempt count.** The press
/// that is not an interrupt ends the pause early and the sequence runs on: the next
/// failure is attempt 2 of the same budget, not a fresh sequence's attempt 1.
#[tokio::test(start_paused = true)]
async fn continue_skips_the_wait_and_keeps_the_attempt_count() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0..2 => Canned::json(408, TIMED_OUT),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until("the loop is waiting to retry", || {
            session.upstream_retry_for_test().get().is_some()
        })
        .await;
    // Most of the first pause passes with the loop still waiting.
    tokio::time::sleep(FIRST_PAUSE * 9 / 10).await;
    assert!(
        session.upstream_retry_for_test().get().is_some(),
        "the loop waits out the pause"
    );
    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue skips the wait"
    );
    session
        .wait_until("the second attempt fails and is scheduled", || {
            session
                .upstream_retry_for_test()
                .get()
                .is_some_and(|retry| retry.attempt == 2)
        })
        .await;
    assert_eq!(
        session.request_continue(Default::default()),
        Ok(()),
        "continue skips the second wait too"
    );
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert_eq!(
        upstream.count(),
        3,
        "two failed attempts and the one that answered"
    );
    host.shut_down().await;
}

/// **A press at idle cuts no later wait.** The interrupt's wake is delivered only to a
/// wait already armed: fired with the loop idle it lands nowhere, so the next turn's
/// failed request waits out its whole pause and the attempt it scheduled goes out.
#[tokio::test(start_paused = true)]
async fn a_press_at_idle_cuts_no_later_retry_wait() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::json(408, TIMED_OUT),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    // Nothing to stop at idle: the press stays latched until a turn starts.
    session.interrupt(Default::default());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until("the loop is waiting to retry", || {
            session.upstream_retry_for_test().get().is_some()
        })
        .await;
    // Most of the pause passes with the loop still waiting: the press at idle armed
    // no wait, so it cut nothing.
    tokio::time::sleep(FIRST_PAUSE * 9 / 10).await;
    assert!(
        session.upstream_retry_for_test().get().is_some(),
        "the wait runs its course: {:?}",
        session.loop_phase()
    );
    session
        .wait_until_with(&upstream, "the scheduled attempt goes out", || {
            upstream.count() == 2
        })
        .await;
    session
        .wait_until("the turn finishes", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert_eq!(upstream.count(), 2, "the retry ran after the press at idle");
    host.shut_down().await;
}
