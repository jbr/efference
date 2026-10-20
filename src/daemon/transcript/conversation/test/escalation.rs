//! An escalated call reads with its escalations on its line, before its decision
//! (docs/control-api.md, docs/permissions.md).

use super::*;

/// A call `parent` escalated to `grand`, which allowed it.
fn escalated_then_allowed() -> Fixture {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![bash("toolu_1", "curl -sI x", "fetch headers")],
    ));
    log.add(escalation(
        "toolu_1",
        Some("session[parent]"),
        Some("grand"),
        Some("is example.com in scope?"),
    ));
    log.add(decision(
        "toolu_1",
        Approval::Approve,
        Some("session[grand]"),
    ));
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text(lines(3)),
        WireState::Raw,
    ));
    log
}

/// The escalation is on the call's line, its escalator and new reviewer named for the
/// reader, and the decision follows it.
#[test]
fn an_escalation_is_on_the_calls_line() {
    let log = escalated_then_allowed();
    assert_eq!(
        whole(&log, &child(Some("grand"))),
        "transcript of child, seq 0–3\n\
         0  bash `curl -sI x` — fetch headers → escalated by parent to you: \"is example.com \
         in scope?\" → allowed by you → ok, 3 lines, it saw all of it\n"
    );
    assert_eq!(
        whole(&log, &child(Some("parent"))),
        "transcript of child, seq 0–3\n\
         0  bash `curl -sI x` — fetch headers → escalated by you to grand: \"is example.com \
         in scope?\" → allowed by grand → ok, 3 lines, it saw all of it\n"
    );
    // The escalation in view without its call: a line of its own under the call's handle.
    assert_eq!(
        read(&log, 1, 3, &child(Some("grand"))),
        "transcript of child, seq 1–3\n\
         0  escalated by parent to you: \"is example.com in scope?\"\n\
         0  allowed by you\n\
         0  result: ok, 3 lines, it saw all of it\n"
    );
}

/// A call still held after an escalation reads as waiting, after each escalation; one
/// that reached the operator names the operator.
#[test]
fn a_held_call_reads_its_escalations_then_waits() {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![bash("toolu_1", "curl -sI x", "fetch headers")],
    ));
    log.add(escalation(
        "toolu_1",
        Some("session[parent]"),
        Some("grand"),
        None,
    ));
    log.add(escalation(
        "toolu_1",
        Some("session[grand]"),
        None,
        Some("outside what I was given"),
    ));
    let reading = Reading {
        held: Some("toolu_1"),
        ..child(None)
    };
    assert_eq!(
        whole(&log, &reading),
        "transcript of child, seq 0–2\n\
         0  bash `curl -sI x` — fetch headers → escalated by parent to grand → escalated by \
         grand to the operator: \"outside what I was given\" → waiting for a decision\n"
    );
}
