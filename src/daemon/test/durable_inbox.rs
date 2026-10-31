//! Arrival-time notifications against a real turn loop and a scripted upstream
//! (docs/inbox.md, docs/resumption.md): a notification no triage has shown is offered at
//! the next one, whether it arrived before a restart or while a triage request was open,
//! and a must-wake notification keeps its flag across the restart.

use super::resumed_batch::start;
use super::upstream_hold::start_with;
use super::*;
use crate::log::{AssistantOutput, ContentBlock, Disposition, Notification, Seq, Triage};
use crate::testing::upstream::{Canned, ScriptedUpstream, Turnstile, end_turn, tool_call};

/// An idle conversation (a turn answered), as a session that stopped between turns leaves it.
fn quiet_log() -> Log {
    let mut log = Log::in_memory();
    log.append(1, user("go")).unwrap();
    log.append(
        1,
        Payload::assistant(AssistantOutput::from_legacy_messages(
            vec![],
            vec![ContentBlock::Text("done".into())],
            vec![],
        )),
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
#[tokio::test(start_paused = true)]
async fn a_resume_over_two_unshown_messages_waits_and_then_offers_both_with_the_new_one() {
    let mut log = quiet_log();
    let first = arrive(
        &mut log,
        Notification::new("agent:reviewer", "please review the diff", None),
    );
    let second = arrive(&mut log, Notification::new("ci", "build 42 failed", None));
    let upstream = ScriptedUpstream::new(|_| Canned::stream(leave_it()));
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());
    assert!(session.has_pending_notifications());

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        upstream.count(),
        0,
        "a resume starts no triage over what the log restored"
    );
    assert!(triages(&session).is_empty());

    let third = session
        .notify(Notification::new("agent:peer", "new notice", None))
        .unwrap();
    session
        .wait_until("the idle triage ran", || !triages(&session).is_empty())
        .await;
    let body = upstream.requests()[0].body.clone();
    assert!(body.contains("please review the diff"), "{body}");
    assert!(body.contains("build 42 failed"), "{body}");
    assert!(body.contains("new notice"), "{body}");
    assert!(body.contains(&format!("[{first}]")) && body.contains(&format!("[{second}]")));
    assert_eq!(triages(&session)[0].shown_through, Some(third));

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(upstream.count(), 1, "shown once, no respin");
    assert!(!session.has_pending_notifications());
    host.shut_down().await;
}

/// **A notification that arrives while a triage request is open** is not counted as shown
/// by it: the idle wake fires again and the next triage offers it as new.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_arrival_during_a_triage_request_is_shown_by_the_next_one() {
    let mut log = quiet_log();
    let early = arrive(&mut log, Notification::new("ci", "early notice", None));
    let request = Turnstile::new();
    let held = request.clone();
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(leave_it()).held(&held),
        _ => Canned::stream(leave_it()),
    });
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());

    // The restored notification does not open a triage; one arriving now does.
    let opening = session
        .notify(Notification::new("agent:peer", "opening notice", None))
        .unwrap();
    session
        .wait_until_with(&upstream, "the first triage request is open", || {
            upstream.count() >= 1
        })
        .await;
    let late = session
        .notify(Notification::new("agent:peer", "late notice", None))
        .unwrap();
    request.open();

    session
        .wait_until("the second triage ran", || triages(&session).len() >= 2)
        .await;
    let bodies: Vec<String> = upstream.requests().into_iter().map(|r| r.body).collect();
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
#[tokio::test(start_paused = true)]
async fn a_must_wake_message_still_pins_after_a_restart() {
    let mut log = quiet_log();
    let seq = arrive(
        &mut log,
        Notification::new("child", "stuck at a gate", None).waking(),
    );
    assert_eq!(seq, 2, "the scripted verdict below names this seq");
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("t1", "triage", r#"{"inbox":{"dismiss":[2]}}"#)),
        _ => Canned::stream(end_turn()),
    });
    let (host, runtime, _dir) = start(&upstream, log).await;
    let session = Arc::clone(runtime.session());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(upstream.count(), 0, "the resume starts nothing");
    session.notify(Notification::new("ci", "unrelated", None));

    session
        .wait_until_with(&upstream, "the pin woke a turn", || upstream.count() >= 2)
        .await;
    assert!(
        triages(&session)[0]
            .inbox
            .contains(&(seq, Disposition::Pin))
    );
    assert!(
        upstream.requests()[1].body.contains("stuck at a gate"),
        "the turn carries the pinned message"
    );
    host.shut_down().await;
}

/// The same for the rider a tool result's triage carries: what arrived before the rider
/// opened is shown and the triage records it as its mark; what arrives during the request
/// reads as new at the next triage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_result_triage_marks_what_its_rider_held() {
    let (model, triage) = (Turnstile::new(), Turnstile::new());
    let held = (model.clone(), triage.clone());
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)).held(&held.0),
        1 => Canned::stream(tool_tri()).held(&held.1),
        2 => Canned::stream(end_turn()),
        _ => Canned::stream(leave_it()),
    });
    let (host, runtime, _dir) = start_with(&upstream, |boot| boot.log = quiet_log()).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go again", "s1"));

    session
        .wait_until_with(&upstream, "the model request is open", || {
            upstream.count() >= 1
        })
        .await;
    let early = session
        .notify(Notification::new("ci", "early notice", None))
        .unwrap();
    model.open();
    session
        .wait_until_with(&upstream, "the result triage request is open", || {
            upstream.count() >= 2
        })
        .await;
    let late = session
        .notify(Notification::new("agent:peer", "late notice", None))
        .unwrap();
    triage.open();

    session
        .wait_until("the idle triage over the late one ran", || {
            triages(&session)
                .iter()
                .any(|t| t.shown_through == Some(late))
        })
        .await;
    let bodies: Vec<String> = upstream.requests().into_iter().map(|r| r.body).collect();
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
