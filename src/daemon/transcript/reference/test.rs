use super::super::fixtures::{Fixture, ask, bash, turn};
use super::*;
use crate::log::RawResult;

/// A handle names one call: a lone call by its bare seq or `.1`, one of several by
/// `seq.n`; a seq with several calls names none of them by itself.
#[test]
fn a_handle_names_the_call_at_it() {
    let mut log = Fixture::new();
    log.user("go", None);
    let lone = log.add(turn("", vec![bash("toolu_a", "ls", "list")]));
    let pair = log.add(turn(
        "",
        vec![ask("toolu_q", "Keep it?"), bash("toolu_b", "pwd", "where")],
    ));
    let at = |text: &str| call_id_at(log.entries(), Handle::parse(text).unwrap());
    assert_eq!(at(&lone.to_string()).as_deref(), Some("toolu_a"));
    assert_eq!(at(&format!("{lone}.1")).as_deref(), Some("toolu_a"));
    assert_eq!(at(&format!("{pair}.1")).as_deref(), Some("toolu_q"));
    assert_eq!(at(&format!("{pair}.2")).as_deref(), Some("toolu_b"));
    assert_eq!(at(&pair.to_string()), None, "two calls share the seq");
    assert_eq!(at("0"), None, "a user turn issues no call");
}

/// A question is found with its answer once one is recorded: who sent it and what it
/// said. A call that is not a question says what it is.
#[test]
fn a_question_carries_its_answer() {
    let mut log = Fixture::new();
    log.user("go", None);
    let seq = log.add(turn("", vec![ask("toolu_q", "Delete the old migrations?")]));
    assert_eq!(
        referenced(log.entries(), "toolu_q"),
        Referenced::Question {
            handle: seq.to_string(),
            question: "Delete the old migrations?".into(),
            answer: None,
        }
    );
    log.submitted(
        Payload::tool_result("toolu_q", RawResult::text("no, keep them"), WireState::Raw),
        Some("session[parent]"),
    );
    assert_eq!(
        referenced(log.entries(), "toolu_q"),
        Referenced::Question {
            handle: seq.to_string(),
            question: "Delete the old migrations?".into(),
            answer: Some(Answer {
                source: Some("session[parent]".into()),
                text: "no, keep them".into(),
                is_error: false,
            }),
        }
    );

    let call = log.add(turn("", vec![bash("toolu_b", "ls", "list")]));
    assert_eq!(
        referenced(log.entries(), "toolu_b"),
        Referenced::Call {
            handle: call.to_string(),
            tool: "bash".into(),
        }
    );
    assert_eq!(referenced(log.entries(), "toolu_none"), Referenced::Nothing);
}
