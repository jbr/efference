//! A held call as each reader reads it: who decides it, why it was held, and the
//! moves that decide it (docs/tools.md, docs/permissions.md).

use super::*;

/// A decision-rung cause with no precedent: the reviewer is told the call, why it was
/// held in plain words, and the moves that decide it; a driver is told who decides.
#[test]
fn a_gate_held_for_scope_with_no_precedent() {
    let scope = Axes {
        in_scope: 0.21,
        ..axes(0, Some(precedent(0, 0, Some(1))))
    };
    let spot = spot(
        Origin::Model,
        "level 2 (scope fit, 21%): needs 1 approved call like it, found none.",
        Some(scope),
    );
    let lines = "intent: fetch only the response headers\n\
        held because: its reviewer is unsure this fits the session's scope; no earlier call \
        like it was allowed";
    assert_eq!(
        rendered(&gated(&spot, supervisor("parent")), &reviewer()),
        format!(
            "child: waiting for your decision on bash `curl -sI https://example.com` (seq \
             8.2)\n{lines}\n{DECIDE}\n{ESCALATE}"
        )
    );
    assert_eq!(
        rendered(&gated(&spot, Reviewer::Operator), &driver()),
        format!(
            "child: waiting for a decision from the operator on bash `curl -sI \
             https://example.com` (seq 8.2)\n{lines}"
        )
    );
}

/// Precedents short of the requirement say how many there were and how many it needs.
#[test]
fn a_gate_held_with_precedents_short_of_the_requirement() {
    let spot = spot(
        Origin::Model,
        "level 3 (sketch, 80%): needs 2 approved calls like it, found 1.",
        Some(axes(3, Some(precedent(3, 1, Some(2))))),
    );
    let reason = "held because: its reviewer rates it serious: hard to undo, or reaching other \
        people; only 1 earlier call like it was allowed, and a call like this needs 2";
    let first = "bash `curl -sI https://example.com` (seq 8.2)\n\
        intent: fetch only the response headers";
    assert_eq!(
        rendered(&gated(&spot, supervisor("parent")), &reviewer()),
        format!("child: waiting for your decision on {first}\n{reason}\n{DECIDE}\n{ESCALATE}")
    );
    assert_eq!(
        rendered(&gated(&spot, supervisor("boss")), &driver()),
        format!("child: waiting for a decision from boss on {first}\n{reason}")
    );
}

/// The prose rung's reason is shown as it wrote it; a fail-closed verdict says the
/// reviewer could not judge the call.
#[test]
fn a_prose_reason_and_a_fail_closed_one() {
    let prose = spot(Origin::Model, "outside the granted paths.", None);
    let first = "bash `curl -sI https://example.com` (seq 8.2)\n\
        intent: fetch only the response headers";
    assert_eq!(
        rendered(&gated(&prose, supervisor("parent")), &reviewer()),
        format!(
            "child: waiting for your decision on {first}\nheld because: outside the granted \
             paths.\n{DECIDE}\n{ESCALATE}"
        )
    );
    assert_eq!(
        rendered(&gated(&prose, Reviewer::Operator), &driver()),
        format!(
            "child: waiting for a decision from the operator on {first}\nheld because: \
             outside the granted paths."
        )
    );

    let failed = spot(
        Origin::Fallback,
        "spotter unavailable — defaulting to the human gate.",
        None,
    );
    assert_eq!(
        rendered(&gated(&failed, supervisor("parent")), &reviewer()),
        format!(
            "child: waiting for your decision on {first}\nheld because: its reviewer could not \
             judge it\n{DECIDE}\n{ESCALATE}"
        )
    );
    assert_eq!(
        rendered(&gated(&failed, supervisor("parent")), &driver()),
        format!(
            "child: waiting for a decision from you on {first}\nheld because: its reviewer \
             could not judge it"
        ),
        "a driver that is also the reviewer, read as a driver, is named as the decider"
    );
}

/// The supervisor reading a gate the dial routed to the operator: the operator is asked,
/// and the reader may decide it as well, with the same call a reviewer is given.
#[test]
fn a_gate_the_supervisor_may_decide_while_the_operator_is_asked() {
    let prose = spot(Origin::Model, "outside the granted paths.", None);
    let may_decide = Reader::notification(Relation::MayDecide, "parent");
    let call = DECIDE.strip_prefix("decide: ").unwrap();
    assert_eq!(
        rendered(&gated(&prose, Reviewer::Operator), &may_decide),
        format!(
            "child: waiting for a decision from the operator or you on bash `curl -sI \
             https://example.com` (seq 8.2)\nintent: fetch only the response headers\nheld \
             because: outside the granted paths.\ndecide: the operator is asked; as child's \
             supervisor you may decide it too: {call}"
        )
    );
}

/// How a session stands to another's status: a gate routed to it, it reviews; one routed
/// elsewhere, the supervisor may decide and any other driver reads who decides; off a
/// gate, the supervisor is a driver.
#[test]
fn a_sessions_relation_follows_the_routing_and_the_supervisor() {
    let prose = spot(Origin::Model, "outside the granted paths.", None);
    let to_parent = gated(&prose, supervisor("parent"));
    let to_operator = gated(&prose, Reviewer::Operator);
    let idle = view(Condition::Idle { closing: None });
    let cases = [
        (&to_parent, "parent", true, true, Relation::Reviewer),
        (&to_operator, "parent", true, true, Relation::MayDecide),
        (&to_operator, "grandparent", false, true, Relation::Driver),
        (&to_operator, "peer", false, false, Relation::Observer),
        (&idle, "parent", true, true, Relation::Driver),
        (&idle, "peer", false, false, Relation::Observer),
    ];
    for (view, me, supervises, drives, expected) in cases {
        assert_eq!(
            Relation::of_session(view, me, supervises, drives),
            expected,
            "{me}"
        );
    }
}

/// A summary that quotes backticks is fenced by a longer run, so it stays one code span
/// beside the intent and the seq.
#[test]
fn a_summary_with_backticks_is_one_code_span() {
    let held = view(Condition::Gated(HeldCall {
        call_id: "toolu_1".into(),
        tool: "agents__create".into(),
        summary: Some("message=\"check `ls ~/.ssh` first\"".into()),
        intent: None,
        handle: Some("8".into()),
        held_because: held_because(&spot(Origin::Model, "level 2", None)),
        reviewer: Reviewer::Operator,
        escalations: Vec::new(),
    }));
    let text = rendered(&held, &driver());
    assert!(
        text.starts_with(
            "child: waiting for a decision from the operator on agents__create \
             ``message=\"check `ls ~/.ssh` first\"`` (seq 8)"
        ),
        "{text}"
    );
}
