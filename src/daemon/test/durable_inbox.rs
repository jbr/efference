//! Arrival-time notifications against a real turn loop and a scripted upstream
//! (docs/inbox.md, docs/resumption.md): a notification no triage has shown is offered at
//! the next one, whether it arrived before a restart or while a triage request was open,
//! and a must-wake notification keeps its flag across the restart.

use super::loop_phase::Latch;
use super::resumed_batch::{requests_script, start};
use super::upstream_hold::{Answer, end_turn, start_with, tool_call, wait_until};
use super::*;
use crate::log::{AssistantOutput, ContentBlock, Disposition, Notification, Seq, Triage};
use std::sync::atomic::Ordering;

/// An idle conversation (a turn answered), as a session that stopped between turns leaves it.
fn quiet_log() -> Log {
    let mut log = Log::in_memory();
    log.append(1, user("go")).unwrap();
    log.append(
        1,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text("done".into())],
                vec![],
            ),
            upstream_failure: None,
        },
    )
    .unwrap();
    log
}

/// An idle triage's verdict that mentions nothing: every notification is left.
fn leave_it() -> String {
    tool_call("t1", "triage", r#"{"inbox":{}}"#)
}

fn arrive(log: &mut Log, n: Notification) -> Seq {
    log.append(1, Payload::Ambient(n)).unwrap()
}

fn triages(session: &Session) -> Vec<Triage> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Triage(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// **Two messages logged before a restart** start nothing on resume; a notification that
/// arrives afterward wakes the idle triage, which offers all three together and records that
/// it showed them, and then nothing wakes the loop again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_over_two_unshown_messages_waits_and_then_offers_both_with_the_new_one() {
    let mut log = quiet_log();
    let first = arrive(
        &mut log,
        Notification::new("agent:reviewer", "please review the diff", None),
    );
    let second = arrive(&mut log, Notification::new("ci", "build 42 failed", None));
    let (requests, bodies, addr) = requests_script(|_| Answer::Stream(leave_it()));
    let (host, runtime) = start(addr, log);
    let session = Arc::clone(runtime.session());
    assert!(session.has_pending_notifications());

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "a resume starts no triage over what the log restored"
    );
    assert!(triages(&session).is_empty());

    let third = session
        .notify("agent:peer".into(), "new notice".into(), None)
        .unwrap();
    wait_until("the idle triage ran", || !triages(&session).is_empty()).await;
    let body = bodies.lock().unwrap()[0].clone();
    assert!(body.contains("please review the diff"), "{body}");
    assert!(body.contains("build 42 failed"), "{body}");
    assert!(body.contains("new notice"), "{body}");
    assert!(body.contains(&format!("[{first}]")) && body.contains(&format!("[{second}]")));
    assert_eq!(triages(&session)[0].shown_through, Some(third));

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(requests.load(Ordering::SeqCst), 1, "shown once, no respin");
    assert!(!session.has_pending_notifications());
    host.shut_down().await;
}

/// **A notification that arrives while a triage request is open** is not counted as shown
/// by it: the idle wake fires again and the next triage offers it as new.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_arrival_during_a_triage_request_is_shown_by_the_next_one() {
    static REQUEST: Latch = Latch::new();
    let mut log = quiet_log();
    let early = arrive(&mut log, Notification::new("ci", "early notice", None));
    let (requests, bodies, addr) = requests_script(|n| {
        if n == 0 {
            REQUEST.wait();
        }
        Answer::Stream(leave_it())
    });
    let (host, runtime) = start(addr, log);
    let session = Arc::clone(runtime.session());

    // The restored notification does not open a triage; one arriving now does.
    let opening = session
        .notify("agent:peer".into(), "opening notice".into(), None)
        .unwrap();
    wait_until("the first triage request is open", || {
        requests.load(Ordering::SeqCst) >= 1
    })
    .await;
    let late = session
        .notify("agent:peer".into(), "late notice".into(), None)
        .unwrap();
    REQUEST.open();

    wait_until("the second triage ran", || triages(&session).len() >= 2).await;
    let bodies = bodies.lock().unwrap().clone();
    assert!(bodies[0].contains("early notice") && bodies[0].contains("opening notice"));
    assert!(!bodies[0].contains("late notice"));
    assert!(bodies[1].contains("late notice"), "{}", bodies[1]);
    let marks: Vec<_> = triages(&session).iter().map(|t| t.shown_through).collect();
    assert_eq!(marks, [Some(opening), Some(late)]);
    assert!(early < opening, "the restored notification came first");
    host.shut_down().await;
}

/// **A must-wake message from before a restart** still pins at its first show whatever the
/// triage says (the first show being the triage a later arrival starts), and the pin wakes
/// the idle agent into a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_must_wake_message_still_pins_after_a_restart() {
    let mut log = quiet_log();
    let seq = arrive(
        &mut log,
        Notification::new("child", "stuck at a gate", None).waking(),
    );
    assert_eq!(seq, 2, "the scripted verdict below names this seq");
    let (requests, bodies, addr) = requests_script(|n| match n {
        0 => Answer::Stream(tool_call("t1", "triage", r#"{"inbox":{"dismiss":[2]}}"#)),
        _ => Answer::Stream(end_turn()),
    });
    let (host, runtime) = start(addr, log);
    let session = Arc::clone(runtime.session());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "the resume starts nothing"
    );
    session.notify("ci".into(), "unrelated".into(), None);

    wait_until("the pin woke a turn", || {
        requests.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(
        triages(&session)[0]
            .inbox
            .contains(&(seq, Disposition::Pin))
    );
    assert!(
        bodies.lock().unwrap()[1].contains("stuck at a gate"),
        "the turn carries the pinned message"
    );
    host.shut_down().await;
}

/// The same for the rider a tool result's triage carries: what arrived before the rider
/// opened is shown and the triage records it as its mark; what arrives during the request
/// reads as new at the next triage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_result_triage_marks_what_its_rider_held() {
    static MODEL: Latch = Latch::new();
    static TRIAGE: Latch = Latch::new();
    let (requests, bodies, addr) = requests_script(|n| match n {
        0 => {
            MODEL.wait();
            Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#))
        }
        1 => {
            TRIAGE.wait();
            Answer::Stream(tool_tri())
        }
        2 => Answer::Stream(end_turn()),
        _ => Answer::Stream(leave_it()),
    });
    let (host, runtime) = start_with(addr, |boot| boot.log = quiet_log());
    let session = Arc::clone(runtime.session());
    session.submit_turn(crate::frontend::SubmittedTurn::new("go again", "s1"));

    wait_until("the model request is open", || {
        requests.load(Ordering::SeqCst) >= 1
    })
    .await;
    let early = session
        .notify("ci".into(), "early notice".into(), None)
        .unwrap();
    MODEL.open();
    wait_until("the result triage request is open", || {
        requests.load(Ordering::SeqCst) >= 2
    })
    .await;
    let late = session
        .notify("agent:peer".into(), "late notice".into(), None)
        .unwrap();
    TRIAGE.open();

    wait_until("the idle triage over the late one ran", || {
        triages(&session)
            .iter()
            .any(|t| t.shown_through == Some(late))
    })
    .await;
    let bodies = bodies.lock().unwrap().clone();
    assert!(bodies[1].contains("early notice") && !bodies[1].contains("late notice"));
    let result_triage = triages(&session)
        .into_iter()
        .find(|t| t.for_result.is_some())
        .expect("a result triage");
    assert_eq!(result_triage.shown_through, Some(early));
    host.shut_down().await;
}

/// A `retain` verdict that keeps the whole result and mentions no notification.
fn tool_tri() -> String {
    tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)
}
