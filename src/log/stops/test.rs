//! The classifier's edges are all in one place: what counts as a reply that
//! supplied nothing. Everything else is bookkeeping over the log's own shapes.

use super::*;
use crate::log::{AssistantOutput, ContentBlock, Payload, ToolCall};

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq: seq as crate::log::Seq,
        turn: seq as crate::log::Turn,
        at: crate::log::Timestamp(0),
        submission_id: None,
        source: None,
        payload,
    }
}

fn said(text: &str) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            Vec::new(),
            vec![ContentBlock::Text(text.to_string())],
            Vec::new(),
        ),
        upstream_failure: None,
    }
}

/// A turn that called a tool: the loop keeps going, so it is not an ending.
fn called(text: &str) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            Vec::new(),
            vec![ContentBlock::Text(text.to_string())],
            vec![ToolCall::new(
                "bash",
                "call_1",
                serde_json::json!({"command": "ls"}),
            )],
        ),
        upstream_failure: None,
    }
}

fn replied(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.to_string())])
}

fn classify_pairs(pairs: &[Payload]) -> Vec<Stop> {
    let entries: Vec<_> = pairs
        .iter()
        .enumerate()
        .map(|(i, p)| entry(i as u64, p.clone()))
        .collect();
    classify(&entries)
        .into_iter()
        .map(|(stop, _)| stop)
        .collect()
}

/// The failure the count exists to measure: the model stopped without asking
/// anything, and the next turn told it nothing it did not already have.
#[test]
fn a_stop_the_next_turn_answered_with_nothing_is_barren() {
    assert_eq!(
        classify_pairs(&[
            said("I'm proceeding with resolver wiring now; no input needed."),
            replied("👍"),
        ]),
        [Stop::Barren]
    );
}

/// Every acknowledgement spelling jbr actually used in the stalled session, plus
/// the ones a supervising agent used. All bought the same nothing.
#[test]
fn acknowledgements_in_any_arrangement_are_barren() {
    for reply in [
        "👍",
        "💯",
        "🚀",
        "ok",
        "Sounds good!",
        "sounds good, carry on!",
        "Great! Please continue",
        "Nice! Carry on!",
        "Sweet, carry on",
        "Continue.",
        "yep — keep going",
    ] {
        assert_eq!(
            classify_pairs(&[said("Continuing."), replied(reply)]),
            [Stop::Barren],
            "{reply:?}"
        );
    }
}

/// The safe direction: anything that could be an instruction counts as answered,
/// so the barren number understates rather than overstates.
#[test]
fn a_reply_carrying_anything_at_all_is_answered() {
    for reply in [
        "carry on, but commit first",
        "no, do the other one",
        "stop",
        "wait",
        "Continue with the resolver, then run the gates.",
        "Do not commit yet. Diff inspection found contract gaps.",
        "yes — and add a test",
        "sounds good. one thing though",
    ] {
        assert_eq!(
            classify_pairs(&[said("Continuing."), replied(reply)]),
            [Stop::Answered],
            "{reply:?}"
        );
    }
}

/// A barren stop keeps the label of whoever answered it, so a supervising agent
/// nudging a session reads apart from the operator doing it.
#[test]
fn a_stop_keeps_the_label_of_whoever_answered_it() {
    let mut nudge = entry(1, replied("Continue."));
    nudge.source = Some("agent[pale-quail]".to_string());
    let entries = vec![entry(0, said("Continuing.")), nudge];
    assert_eq!(
        classify(&entries),
        [(Stop::Barren, Some("agent[pale-quail]".to_string()))]
    );
}

/// A turn ending on a question is licensed whatever the answer was: the log cannot
/// say whether it needed asking, so it is counted and not faulted.
#[test]
fn a_parting_question_is_its_own_arm() {
    assert_eq!(
        classify_pairs(&[said("Should I commit here, or keep going?"), replied("👍"),]),
        [Stop::Question]
    );
}

/// A question mark inside the prose is not a question put to the operator — it is
/// usually quoted or rhetorical. Only the last thing said hands the turn over.
#[test]
fn only_a_trailing_question_mark_counts() {
    assert_eq!(
        classify_pairs(&[
            said("The route answers \"what can the host resolve?\" — wiring it now."),
            replied("👍"),
        ]),
        [Stop::Barren]
    );
}

/// A call still to run is the loop continuing, not an ending — which is also how a
/// call held at a permission gate stays out of the count.
#[test]
fn a_turn_that_called_a_tool_is_not_a_stop() {
    assert_eq!(classify_pairs(&[called("Continuing."), replied("👍")]), []);
}

/// Nothing after the last stop: the session ended there. Finished or abandoned —
/// the log does not say, and neither does this.
#[test]
fn a_stop_with_nothing_after_it_is_final() {
    assert_eq!(
        classify_pairs(&[said("Continuing."), replied("go on"), said("Done.")]),
        [Stop::Barren, Stop::Final]
    );
}

/// The stalled session's actual shape, abbreviated: six endings, none of them
/// asking anything, each answered with an acknowledgement.
#[test]
fn the_recorded_stall_reads_as_a_run_of_barren_stops() {
    let mut payloads = Vec::new();
    for reply in ["👍", "👍", "💯", "🚀"] {
        payloads.push(said(
            "I'm proceeding with resolver wiring now; no input needed.",
        ));
        payloads.push(replied(reply));
    }
    let stops = classify_pairs(&payloads);
    assert_eq!(stops.len(), 4);
    assert!(stops.iter().all(|stop| *stop == Stop::Barren));
}

/// A failed request leaves a record-only assistant entry with no calls. It is not
/// something the model said, so a reply to it is not a stop — otherwise one
/// exhausted quota, answered on every retry, reads as a run of barren stops.
#[test]
fn an_upstream_failure_is_not_a_stop() {
    let failed = Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(Vec::new(), Vec::new(), Vec::new()),
        upstream_failure: Some("upstream API returned 429 Too Many Requests".into()),
    };
    assert_eq!(
        classify_pairs(&[called("Writing DONE.md."), failed, replied("sounds good!")]),
        []
    );
}

/// An assistant turn that put a question through `ask`, and the result that answered it.
fn asked(question: &str, id: &str) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            Vec::new(),
            Vec::new(),
            vec![ToolCall::new(
                crate::supervisor::ASK,
                id,
                serde_json::json!({ "question": question }),
            )],
        ),
        upstream_failure: None,
    }
}

fn answer(id: &str, text: &str) -> Payload {
    Payload::tool_result(
        id,
        crate::log::RawResult::text(text),
        crate::log::WireState::Raw,
    )
}

/// An ask is graded by its reply with the turn ending's own test, and an ask is not a
/// turn ending: `classify` does not count it.
#[test]
fn an_ask_is_graded_by_what_its_reply_carried() {
    let entries = vec![
        entry(0, asked("Proceeding with X, OK?", "a1")),
        entry(1, answer("a1", "sounds good!")),
        entry(2, asked("Which branch?", "a2")),
        entry(3, answer("a2", "use the release branch")),
    ];
    assert_eq!(
        classify_asks(&entries),
        [(Some(Stop::Barren), None), (Some(Stop::Answered), None)]
    );
    assert!(classify(&entries).is_empty(), "an ask is not a turn ending");
}

/// The reply's label is the result entry's `source`, as a turn's is the `User` entry's.
#[test]
fn an_ask_reply_keeps_the_label_of_whoever_gave_it() {
    let mut reply = entry(1, answer("a1", "ok"));
    reply.source = Some("agent[pale-quail]".to_string());
    let entries = vec![entry(0, asked("Proceed?", "a1")), reply];
    assert_eq!(
        classify_asks(&entries),
        [(Some(Stop::Barren), Some("agent[pale-quail]".to_string()))]
    );
}

/// An interrupted ask (error result, elided on the wire), an errored one, and one the
/// log ends inside are counted and not graded.
#[test]
fn an_interrupted_or_unanswered_ask_is_counted_not_graded() {
    use crate::log::{RawResult, WireState};
    let entries = vec![
        entry(0, asked("Sure?", "a1")),
        entry(
            1,
            Payload::tool_result(
                "a1",
                RawResult::error("ask interrupted by user"),
                WireState::Elided,
            ),
        ),
        entry(2, asked("Again?", "a2")),
        // A reply that raced the interrupt: text kept in the log, withheld on the wire.
        entry(
            3,
            Payload::tool_result("a2", RawResult::text("ok"), WireState::Elided),
        ),
        entry(4, asked("Still there?", "a3")),
    ];
    assert_eq!(
        classify_asks(&entries),
        [(None, None), (None, None), (None, None)]
    );
}

/// Results of other tools, and asks in a record-only (failed) turn, are not asks.
#[test]
fn only_completed_ask_calls_are_counted() {
    let entries = vec![
        entry(0, called("looking")),
        entry(1, answer("call_1", "ok")),
        entry(
            2,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    Vec::new(),
                    Vec::new(),
                    vec![ToolCall::new(
                        crate::supervisor::ASK,
                        "a9",
                        serde_json::json!({ "question": "?" }),
                    )],
                ),
                upstream_failure: Some("boom".into()),
            },
        ),
    ];
    assert!(classify_asks(&entries).is_empty());
}
