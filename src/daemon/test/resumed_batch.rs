//! A resumed session's pending batch against a real turn loop and a scripted upstream
//! (docs/resumption.md): the operator's first action brings the batch back before any
//! model request, a submitted turn waits behind it, and an event does not replay it.

use super::capture::verdict;
use super::runtime::{boot, started, workdir};
use super::*;
use crate::daemon::SessionRuntime;
use crate::frontend::SubmittedTurn;
use crate::record::log::{Approval, Telemetry};
use crate::record::log::{AssistantOutput, CallId, RawResult, Resumed, Spot, Stance, WireState};
use crate::record::project::{ApiBlock, Role};
use crate::spotter::StaticSpotter;
use crate::testing::upstream::{Canned, ScriptedUpstream, end_turn};
use tempfile::TempDir;
use trillium_tokio::Swansong;

fn call(id: &str, command: &str) -> ToolCall {
    ToolCall::new(
        crate::record::names::tools::BASH,
        id,
        serde_json::json!({ "command": command }),
    )
}

/// A log stopped mid-batch: a completion calling `calls`, and what `extra` says happened
/// to them, then the `Resumed` entry naming `pending`.
fn stopped_log(calls: Vec<ToolCall>, pending: &[&str], extra: impl FnOnce(&mut Log)) -> Log {
    let mut log = Log::in_memory();
    log.append(1, user("go")).unwrap();
    log.append(
        1,
        Payload::assistant(AssistantOutput::from_legacy_messages(vec![], vec![], calls)),
    )
    .unwrap();
    extra(&mut log);
    log.append(
        1,
        Payload::Resumed(
            Resumed::new(60_000)
                .with_pending_calls(pending.iter().map(|id| CallId((*id).into())).collect()),
        ),
    )
    .unwrap();
    log
}

fn captured(log: &mut Log, id: &str, output: &str) -> u64 {
    log.append(1, Payload::Telemetry(Telemetry::call_started(id)))
        .unwrap();
    log.append(
        1,
        Payload::tool_result(id, RawResult::text(output), WireState::Pending),
    )
    .unwrap()
}

fn gated(log: &mut Log, id: &str) {
    log.append(
        1,
        Payload::Spot(Spot::new(id, Stance::Gate, "out of scope")),
    )
    .unwrap();
}

/// A resumed session over `log` whose spotter gates every call, pointed at `upstream`
/// and returned once its loop has gone idle, in a working directory of its own that goes
/// when the returned guard drops.
pub(super) async fn start(
    upstream: &ScriptedUpstream,
    log: Log,
) -> (Swansong, SessionRuntime, TempDir) {
    let (cwd, dir) = workdir("resumed");
    let mut boot = boot(&cwd);
    boot.client = Client::new(
        upstream.endpoint(crate::record::log::Provider::Anthropic, "/v1/messages"),
        "m",
        "sys",
    );
    boot.spotter = StaticSpotter {
        stance: Stance::Gate,
        reason: "out of scope".into(),
    };
    boot.log = log;
    let host = Swansong::new();
    let runtime = started(boot, &host).await;
    (host, runtime, dir)
}

fn two_verdicts_then_done(n: usize) -> Canned {
    match n {
        0 => Canned::stream(verdict(&["all", "all"])),
        _ => Canned::stream(end_turn()),
    }
}

/// Ask for the continue until the parked loop takes it.
async fn continue_when_parked(session: &Session) {
    session
        .wait_until("the loop parks and accepts a continue", || {
            session.request_continue(Default::default()).is_ok()
        })
        .await;
}

async fn approve_the_gate(session: &Session, id: &str) {
    session
        .wait_until("the gate reappears", || session.pending_gate().is_some())
        .await;
    assert!(session.resolve_approval(&CallId(id.into()), Approval::Approve, Default::default()));
}

fn results_of(log: &[LogEntry], id: &str) -> usize {
    log.iter()
        .filter(|e| matches!(&e.payload, Payload::ToolResult { call_id, .. } if call_id.0 == id))
        .count()
}

/// **A continue replays the batch.** One call captured its result before the stop and one
/// was held at the gate. Nothing runs until the continue; then the held call is gated
/// again with no model request first, the captured call does not run again, one triage
/// triages both results, and the notice follows them on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_continue_replays_the_batch_through_the_gate() {
    let log = stopped_log(
        vec![call("c1", "printf one"), call("c2", "printf two")],
        &["c1", "c2"],
        |log| {
            captured(log, "c1", "one");
            gated(log, "c2");
        },
    );
    let upstream = ScriptedUpstream::new(two_verdicts_then_done);
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());

    // Opening the session did nothing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(upstream.count(), 0);
    assert!(session.pending_gate().is_none());

    continue_when_parked(&session).await;
    approve_the_gate(&session, "c2").await;
    session
        .wait_until_with(&upstream, "the turn finishes", || {
            upstream.count() >= 2 && session.loop_phase() == LoopPhase::Idle
        })
        .await;
    host.shut_down().await;

    let log = session.snapshot();
    assert_eq!(
        results_of(&log, "c1"),
        1,
        "the captured call did not run again"
    );
    assert_eq!(results_of(&log, "c2"), 1);
    let verdicts = log
        .iter()
        .filter(|e| matches!(e.payload, Payload::ResultVerdict { .. }))
        .count();
    assert_eq!(verdicts, 2, "both results were decided at the one triage");
    let triage = upstream.requests()[0].body.clone();
    assert!(
        triage.contains("one") && triage.contains("two"),
        "the first request is the triage over both results: {triage}"
    );

    // The wire answers both calls, with the notice after the results.
    let messages = session.project_messages();
    let answered = &messages[2];
    assert_eq!(answered.role, Role::User);
    match &answered.content[..] {
        [
            ApiBlock::ToolResult { .. },
            ApiBlock::ToolResult { .. },
            ApiBlock::Text { text, .. },
        ] => assert!(text.contains("stopped with 2 tool calls pending"), "{text}"),
        other => panic!("expected [result, result, notice], got {other:?}"),
    }
}

/// **A captured result gets its verdict at the resumed triage** without its call running
/// again, and with no approval needed: nothing was left to run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_captured_result_is_triaged_without_running_again() {
    let log = stopped_log(vec![call("c1", "printf one")], &["c1"], |log| {
        captured(log, "c1", "one");
    });
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(verdict(&["all"])),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());

    continue_when_parked(&session).await;
    session
        .wait_until_with(&upstream, "the turn finishes", || {
            upstream.count() >= 2 && session.loop_phase() == LoopPhase::Idle
        })
        .await;
    host.shut_down().await;

    let log = session.snapshot();
    assert_eq!(results_of(&log, "c1"), 1);
    assert_eq!(
        log.iter()
            .filter(|e| matches!(e.payload, Payload::Telemetry(Telemetry::CallStarted { .. })))
            .count(),
        1,
        "the call was not dispatched again"
    );
    let captured_at = log
        .iter()
        .find(|e| matches!(e.payload, Payload::ToolResult { .. }))
        .unwrap()
        .seq;
    assert!(log.iter().any(|e| matches!(
        &e.payload,
        Payload::ResultVerdict { for_result, .. } if *for_result == captured_at
    )));
    assert!(upstream.requests()[0].body.contains("one"));
}

/// **A submitted turn waits behind the batch.** It is not appended before the calls'
/// results, since a `User` entry after unanswered `tool_use` blocks breaks the wire; it is
/// delivered after them, and the triage does not carry it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submitted_turn_is_delivered_after_the_batch() {
    let log = stopped_log(
        vec![call("c1", "printf one"), call("c2", "printf two")],
        &["c1", "c2"],
        |log| {
            captured(log, "c1", "one");
            gated(log, "c2");
        },
    );
    let upstream = ScriptedUpstream::new(two_verdicts_then_done);
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());
    session
        .wait_until("the loop parks", || session.is_settled())
        .await;

    session.submit_turn(SubmittedTurn::new("carry on", "s1"));
    approve_the_gate(&session, "c2").await;
    session
        .wait_until_with(&upstream, "the turn finishes", || {
            upstream.count() >= 2 && session.loop_phase() == LoopPhase::Idle
        })
        .await;
    host.shut_down().await;

    let log = session.snapshot();
    let position = |matcher: fn(&Payload) -> bool| log.iter().rposition(|e| matcher(&e.payload));
    let said = position(
        |p| matches!(p, Payload::User(blocks) if format!("{blocks:?}").contains("carry on")),
    )
    .expect("the turn is in the log");
    let last_verdict = position(|p| matches!(p, Payload::ResultVerdict { .. })).unwrap();
    let last_result = position(|p| matches!(p, Payload::ToolResult { .. })).unwrap();
    assert!(
        said > last_verdict && said > last_result,
        "the turn follows the batch: {said} vs {last_result}, {last_verdict}"
    );
    let requests = upstream.requests();
    assert!(
        !requests[0].body.contains("carry on"),
        "the triage is not shown it"
    );
    assert!(
        requests[1].body.contains("carry on"),
        "the next request carries it"
    );
}

/// **An event does not replay the batch.** The notification waits in the inbox, no
/// triage projects the unanswered wire, and nothing runs.
#[tokio::test(start_paused = true)]
async fn an_event_wake_leaves_the_batch_alone() {
    let log = stopped_log(vec![call("c1", "printf one")], &["c1"], |log| {
        gated(log, "c1");
    });
    let upstream = ScriptedUpstream::new(|_| Canned::stream(end_turn()));
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());
    session
        .wait_until("the loop parks", || session.is_settled())
        .await;

    session.notify(Notification::new("watcher", "something happened", None));
    tokio::time::sleep(Duration::from_millis(800)).await;
    host.shut_down().await;

    assert_eq!(upstream.count(), 0, "no request of any kind");
    assert!(session.pending_gate().is_none(), "no call reached the gate");
    assert_eq!(results_of(&session.snapshot(), "c1"), 0);
    assert!(session.has_pending_notifications(), "the event waits");
}
