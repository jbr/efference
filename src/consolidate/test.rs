use super::*;
use crate::log::{AssistantOutput, ContentBlock, LogEntry, Payload, Timestamp};

fn at(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(seq),
        submission_id: None,
        source: None,
        payload,
    }
}
fn user(seq: Seq) -> LogEntry {
    at(seq, Payload::User(vec![ContentBlock::Text("u".into())]))
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

/// `consolidate` is loop-owned, so its brief rides the same path as the process monitor
/// family's (docs/permissions.md); the wire entry keeps the provider shape with no
/// annotations leak.
#[test]
fn consolidate_has_a_brief_and_a_clean_wire_entry() {
    let b = brief(NAME).expect("consolidate brief");
    assert!(b.server.is_none());
    let hints = b.definition.annotations.as_ref().unwrap();
    assert_eq!(hints.read_only_hint, Some(false));
    assert_eq!(hints.open_world_hint, Some(false));
    assert!(brief("bash").is_none());

    let def = definition();
    let keys: Vec<&str> = def
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["description", "input_schema", "name"]);
}

fn responses_call(seq: Seq, call_id: &str) -> LogEntry {
    use crate::log::{CallId, ResponsesOutputItem};
    at(
        seq,
        Payload::Assistant {
            output: AssistantOutput::Responses(vec![ResponsesOutputItem::FunctionCall {
                id: None,
                call_id: CallId(call_id.into()),
                name: "bash".into(),
                arguments: "{}".into(),
                status: None,
            }]),
            excluded: None,
        },
    )
}

fn result(seq: Seq, call_id: &str) -> LogEntry {
    use crate::log::{RawResult, WireState};
    at(
        seq,
        Payload::tool_result(call_id, RawResult::text("out"), WireState::Raw),
    )
}

/// Head-summary pairing checks remain available to the digest and compaction path.
#[test]
fn a_hand_placed_boundary_names_split_calls() {
    let log = vec![
        user(0),
        responses_call(1, "call_a"),
        result(2, "call_a"),
        responses_call(3, "call_b"),
        result(4, "call_b"),
        user(5),
        responses_call(6, "consolidate"),
    ];
    let upto = 4;
    assert_eq!(split_calls(&log, upto), Vec::<CallId>::new());

    // Between call_b and its result: the call is replaced, the output would replay alone.
    assert_eq!(split_calls(&log, 3), vec![CallId("call_b".into())]);
    // Before call_a's result but after nothing else: the same shape at the front.
    assert_eq!(split_calls(&log, 1), vec![CallId("call_a".into())]);
    // A boundary before any call splits nothing.
    assert_eq!(split_calls(&log, 0), Vec::<CallId>::new());
}
