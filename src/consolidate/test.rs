use super::*;
use crate::log::{ContentBlock, LogEntry, Payload, Timestamp};

fn at(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(seq),
        submission_id: None,
        payload,
    }
}
fn user(seq: Seq) -> LogEntry {
    at(seq, Payload::User(vec![ContentBlock::Text("u".into())]))
}
fn work(seq: Seq) -> LogEntry {
    at(
        seq,
        Payload::Assistant {
            thinking: vec![],
            text: vec![],
            calls: vec![],
        },
    )
}

#[test]
fn extent_folds_up_to_just_before_the_live_user_turn() {
    // task A (0..=2), the new task instruction at 3, then the consolidate turn.
    let log = vec![user(0), work(1), work(2), user(3), work(4)];
    assert_eq!(
        extent(&log),
        2,
        "fold task A; preserve the live instruction at seq 3"
    );
}

#[test]
fn summary_parses_and_tolerates_garbage() {
    assert_eq!(
        summary(&json!({"summary": "did X"})).as_deref(),
        Some("did X")
    );
    assert!(summary(&json!({"nope": 1})).is_none());
}

#[test]
fn definition_advertises_the_name_and_a_required_summary() {
    let d = definition();
    assert_eq!(d["name"], json!(NAME));
    assert_eq!(d["input_schema"]["required"], json!(["summary"]));
}
