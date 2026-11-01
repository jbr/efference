//! The interrupt against a real turn loop and a scripted upstream
//! (docs/control-channel.md).

use super::upstream_hold::start;
use super::*;
use crate::frontend::SubmittedTurn;
use crate::inference::testing::upstream::{
    Canned, ScriptedUpstream, Turnstile, end_turn, events, tool_call,
};
use crate::record::log::WireState;

/// A Messages upstream that answers by arrival order: request 0 calls a tool, so the loop
/// runs it and opens a triage; request 1, the triage, waits at `open`, then sends its
/// first event and nothing after it; every later request ends the turn.
fn scripted_upstream(open: &Turnstile) -> ScriptedUpstream {
    let open = open.clone();
    let first_event = events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#
            .to_string(),
    ]);
    ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::stream(first_event.clone()).left_open().held(&open),
        _ => Canned::stream(end_turn()),
    })
}

/// **An interrupt during the triage ends the turn there.** The press ends the triage's
/// read and the results are withheld with a line saying they were interrupted before
/// they were read (nothing blaming triage); the loop then consumes the latch after
/// the triage and goes idle without another request. Left set, the latch rides into the
/// next completion and stops its first call as "aborted by user" — the model's
/// answer to whatever the operator queued to explain the press.
#[tokio::test]
async fn an_interrupt_during_the_triage_ends_the_turn_at_the_gate() {
    // The triage's stream opens as soon as its request arrives.
    let open = Turnstile::new();
    open.open();
    let upstream = scripted_upstream(&open);
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until_with(&upstream, "the triage is open", || upstream.count() == 2)
        .await;
    session.interrupt(crate::record::log::InterruptOrigin {
        source: Some("web".into()),
        control: Some("stop-button".into()),
    });
    session
        .wait_until("the loop is idle", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    // Room for a request the loop should not make. The call that ran is long done, so the
    // clock can be paused for the room (`upstream_hold.rs`).
    tokio::time::pause();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        upstream.count(),
        2,
        "the turn ended at the triage: no request after the interrupted triage"
    );
    assert!(!session.interrupt_pending(), "the press was consumed");
    let log = session.snapshot();
    // The origin is recorded before the signal it describes, so it precedes the
    // triage request's `failed: interrupted` stage.
    let recorded = log
        .iter()
        .position(|e| {
            matches!(
                &e.payload,
                Payload::Telemetry(crate::record::log::Telemetry::Interrupt(origin))
                    if origin.source.as_deref() == Some("web")
                        && origin.control.as_deref() == Some("stop-button")
            )
        })
        .expect("the interrupt is in the log");
    let failed = log
        .iter()
        .position(|e| {
            matches!(
                &e.payload,
                Payload::Telemetry(crate::record::log::Telemetry::RequestStage(stage))
                    if matches!(&stage.stage, crate::record::log::RequestStageKind::Failed { reason }
                        if reason.contains("interrupt"))
            )
        })
        .expect("the interrupted request recorded its failure");
    assert!(recorded < failed, "{recorded} before {failed}");
    // Withheld as at the interrupt's after juncture: the raw stays in the log, captured
    // before the triage, and the verdict that keeps it off the wire names its seq.
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
                wire: WireState::Interrupted(crate::record::log::Interruption { after_ms: None, .. }),
            } if *for_result == captured_seq
        )),
        "the interrupted triage withholds the result it captured"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
}

/// The press lands after the triage's request reached the upstream but before its
/// stream opened — the window `notify_waiters`, which stores no permit, would lose
/// if the client armed its abort only once the stream was open. The upstream holds
/// the triage's response headers until the interrupt has fired, so this ordering is
/// forced rather than left to scheduling: the loop must still end the turn.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_before_the_triage_stream_opens_still_ends_it() {
    let open = Turnstile::new();
    let upstream = scripted_upstream(&open);
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until_with(&upstream, "the triage request reached the upstream", || {
            upstream.count() == 2
        })
        .await;
    session.interrupt(Default::default());
    open.open();
    session
        .wait_until("the loop is idle", || {
            session.loop_phase() == LoopPhase::Idle
        })
        .await;
    assert_eq!(
        upstream.count(),
        2,
        "no request after the interrupted triage"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
}

/// Whether `entry` is a request stage of `origin` matching `stage`.
fn stage_of(
    entry: &LogEntry,
    origin: RequestOrigin,
    stage: impl Fn(&crate::record::log::RequestStageKind) -> bool,
) -> bool {
    matches!(
        &entry.payload,
        Payload::Telemetry(crate::record::log::Telemetry::RequestStage(recorded))
            if recorded.origin == origin && stage(&recorded.stage)
    )
}

/// **A press before the request is armed stops it unsent.** The press lands after the
/// loop's last juncture and before the turn's request arms its abort (`before_request`),
/// where `notify_waiters` has no waiter to wake. The request reads the latch once armed,
/// so it is never sent: the loop records the aborted attempt, consumes the press, and the
/// turn ends there instead of after a response the operator had already stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_before_the_request_is_armed_stops_it_unsent() {
    let upstream = ScriptedUpstream::new(|_| Canned::stream(end_turn()));
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    *session.before_request.lock().unwrap() = Some(Box::new(|session: &Session| {
        session.interrupt(crate::record::log::InterruptOrigin {
            source: Some("web".into()),
            control: Some("stop-button".into()),
        });
    }));
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    session
        .wait_until_with(&upstream, "the turn ended", || session.turns_ended() == 1)
        .await;

    assert_eq!(
        upstream.count(),
        0,
        "the request the press reached before its arm was sent"
    );
    assert!(!session.interrupt_pending(), "the press was consumed");
    let log = session.snapshot();
    let pressed = log
        .iter()
        .position(|entry| {
            matches!(
                &entry.payload,
                Payload::Telemetry(crate::record::log::Telemetry::Interrupt(origin))
                    if origin.control.as_deref() == Some("stop-button")
            )
        })
        .expect("the press is in the log");
    let aborted = log
        .iter()
        .position(|entry| {
            matches!(
                &entry.payload,
                Payload::Assistant { excluded: Some(why), .. }
                    if why.why() == "generation interrupted"
            )
        })
        .expect("the aborted attempt is recorded");
    assert!(pressed < aborted, "{pressed} before {aborted}");
    assert!(
        !log.iter()
            .any(|entry| stage_of(entry, RequestOrigin::Turn, |_| true)),
        "a request that was not sent records no stage"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
}

/// **A press at idle is not the idle triage's.** It has nothing to stop and stays latched
/// until the next turn starts; the idle triage, which runs on the turn's own client
/// meanwhile, answers only to a press during it, so its request goes out and ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_at_idle_leaves_the_idle_triage_alone() {
    let upstream = ScriptedUpstream::new(|_| Canned::stream(end_turn()));
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.interrupt(Default::default());
    session.notify(Notification::new("ci", "the build finished", None));

    session
        .wait_until_with(&upstream, "the idle triage ended", || {
            session.loop_phase() == LoopPhase::Idle
                && session.snapshot().iter().any(|entry| {
                    stage_of(entry, RequestOrigin::Triage, |stage| {
                        *stage == crate::record::log::RequestStageKind::Ended
                    })
                })
        })
        .await;

    assert!(upstream.count() >= 1, "the idle triage's request went out");
    assert!(
        !session.snapshot().iter().any(|entry| {
            stage_of(entry, RequestOrigin::Triage, |stage| {
                matches!(stage, crate::record::log::RequestStageKind::Failed { .. })
            })
        }),
        "the press at idle stopped no triage request"
    );
    assert!(
        session.interrupt_pending(),
        "the press stays latched until a turn starts"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
}

/// **A press during the turn's stream still ends it.** The request found the latch clear
/// before it was sent; the press then wakes the read it armed: the attempt is kept as the
/// aborted one, the press is consumed, and the loop goes idle without another request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_during_the_turns_stream_still_ends_it() {
    let first_event = events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#
            .to_string(),
    ]);
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(first_event.clone()).left_open(),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    session
        .wait_until_with(&upstream, "the turn's stream is open", || {
            session.snapshot().iter().any(|entry| {
                stage_of(entry, RequestOrigin::Turn, |stage| {
                    *stage == crate::record::log::RequestStageKind::FirstByte
                })
            })
        })
        .await;

    session.interrupt(Default::default());
    session
        .wait_until("the turn ended", || session.turns_ended() == 1)
        .await;

    assert_eq!(upstream.count(), 1, "no request after the interrupted one");
    assert!(!session.interrupt_pending(), "the press was consumed");
    assert!(
        session.snapshot().iter().any(|entry| {
            stage_of(entry, RequestOrigin::Turn, |stage| {
                *stage
                    == crate::record::log::RequestStageKind::Failed {
                        reason: "interrupted".into(),
                    }
            })
        }),
        "the turn's request closed as interrupted"
    );

    host.shut_down().await;
    runtime.finished().await.unwrap();
}
