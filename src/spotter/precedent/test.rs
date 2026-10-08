use super::*;
use crate::log::{AssistantOutput, Origin, Spot, Timestamp, ToolOutcome, ToolTelemetry};
use serde_json::json;

fn entry(payload: Payload) -> LogEntry {
    LogEntry {
        seq: 0,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
        source: None,
        payload,
    }
}

fn assistant(calls: Vec<ToolCall>) -> LogEntry {
    entry(Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(vec![], vec![], calls),
        upstream_failure: None,
    })
}

fn call(id: &str, command: &str) -> ToolCall {
    ToolCall::new("bash", id, json!({ "command": command }))
}

fn ran(id: &str) -> LogEntry {
    entry(Payload::ToolTelemetry(ToolTelemetry {
        call_id: id.into(),
        tool: "bash".into(),
        duration_ms: 1,
        outcome: ToolOutcome::Applied,
        result_bytes: 0,
        images: 0,
        bash: None,
        edit: None,
    }))
}

fn spot(id: &str, stance: Stance) -> LogEntry {
    entry(Payload::Spot(Spot {
        call_id: id.into(),
        stance,
        reason: String::new(),
        origin: Origin::Model,
        model: None,
        probed_prefix: None,
        axes: None,
    }))
}

fn ids(calls: &[ToolCall]) -> Vec<&str> {
    calls.iter().map(|c| c.id.0.as_str()).collect()
}

#[test]
fn a_call_no_spotter_judged_is_consented() {
    let log = vec![assistant(vec![call("a", "ls")]), ran("a")];
    assert_eq!(ids(&consented(&log)), ["a"]);
}

#[test]
fn a_held_call_that_ran_is_consented() {
    let log = vec![
        assistant(vec![call("a", "rm x"), call("b", "rm y")]),
        spot("a", Stance::Gate),
        spot("b", Stance::Watch),
        ran("a"),
        ran("b"),
    ];
    assert_eq!(ids(&consented(&log)), ["a", "b"]);
}

/// A clear is the spotter's decision, not the operator's, so it is not evidence
/// for the next clear.
#[test]
fn a_cleared_call_is_not_consented() {
    let log = vec![
        assistant(vec![call("a", "ls"), call("b", "ls -l")]),
        spot("a", Stance::Clear),
        spot("b", Stance::ClearAlways),
        ran("a"),
        ran("b"),
    ];
    assert!(consented(&log).is_empty());
}

#[test]
fn a_call_that_never_ran_is_not_consented() {
    let log = vec![
        assistant(vec![call("a", "rm x"), call("b", "ls")]),
        spot("a", Stance::Gate),
        ran("b"),
    ];
    assert_eq!(ids(&consented(&log)), ["b"]);
}

#[test]
fn consented_calls_come_back_in_log_order() {
    let log = vec![
        assistant(vec![call("a", "ls")]),
        ran("a"),
        assistant(vec![call("b", "pwd"), call("c", "date")]),
        ran("c"),
        ran("b"),
    ];
    assert_eq!(ids(&consented(&log)), ["a", "b", "c"]);
}
