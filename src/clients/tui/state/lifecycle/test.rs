//! `in_flight_call` across both wire dialects: the executing call is read off the
//! unpaired tail of the committed log, whatever shape the provider recorded it in.

use crate::clients::tui::harness::Screenplay;
use crate::record::log::{AssistantOutput, CallId, Payload, ResponsesOutputItem, WireState};
use serde_json::json;

fn in_flight(play: &Screenplay) -> Option<(String, String)> {
    play.state
        .in_flight_call()
        .map(|call| (call.id.0.clone(), call.name.clone()))
}

fn labels(play: &Screenplay) -> Vec<&'static str> {
    play.state.strip_items().iter().map(|e| e.label).collect()
}

/// The same lifecycle, in each dialect: an unpaired call is in flight; its result
/// clears it. The Responses arm parses the argument string into the same semantic
/// call the Messages arm stores directly, so both report one identity.
#[test]
fn an_unpaired_call_is_in_flight_in_either_dialect() {
    let mut messages = Screenplay::new(80, 12);
    messages.user("read it");
    messages.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));
    assert_eq!(
        in_flight(&messages),
        Some(("toolu_r".into(), "read".into())),
        "a Messages tool_use with no result is executing"
    );
    messages.tool_result("toolu_r", "…", false, WireState::Raw);
    assert_eq!(in_flight(&messages), None, "its result pairs it");

    let mut responses = Screenplay::new(80, 12);
    responses.user("read it");
    responses.running_responses_tool("call_r", "read", r#"{"path":"src/log.rs"}"#);
    assert_eq!(
        in_flight(&responses),
        Some(("call_r".into(), "read".into())),
        "a Responses function_call with no result is executing"
    );
    let call = responses.state.in_flight_call().unwrap();
    assert_eq!(
        call.input,
        json!({ "path": "src/log.rs" }),
        "the argument string is parsed into the semantic call"
    );
    responses.tool_result("call_r", "…", false, WireState::Raw);
    assert_eq!(in_flight(&responses), None, "its result pairs it");
}

/// A foreground bash reads as foreground through the Responses arm too, so the
/// control strip offers `background` and `kill` regardless of which provider
/// recorded the call.
#[test]
fn a_responses_bash_call_is_a_foreground_bash() {
    let mut play = Screenplay::new(80, 12);
    play.user("run it");
    play.running_responses_tool(
        "call_b",
        crate::record::names::tools::BASH,
        r#"{"command":"cargo test"}"#,
    );
    let call = play.state.in_flight_call().expect("in flight");
    assert!(crate::tools::bash::is_foreground_call(&call));
    assert_eq!(&labels(&play)[..2], ["background", "kill"]);
}

/// Arguments the provider sent that do not parse are recorded but never executed
/// (`src/agent.rs` makes that turn record-only), so nothing is in flight — and the
/// strip must not promise a `kill` for a process that was never started.
#[test]
fn malformed_responses_arguments_put_nothing_in_flight() {
    let mut play = Screenplay::new(80, 12);
    play.user("run it");
    play.running_responses_tool(
        "call_x",
        crate::record::names::tools::BASH,
        r#"{"command": "cargo te"#,
    );
    assert_eq!(in_flight(&play), None);
    assert_eq!(
        labels(&play)[0],
        "interrupt",
        "the loop is running, but no call is named"
    );
}

/// The multi-call approximation is the same in both dialects: the entry's first
/// call stands for the whole step until a result pairs it.
#[test]
fn a_multi_call_step_reports_its_first_call_in_either_dialect() {
    let mut play = Screenplay::new(80, 12);
    play.user("do both");
    play.commit(Payload::assistant(AssistantOutput::Responses(vec![
        ResponsesOutputItem::FunctionCall {
            id: None,
            call_id: CallId("call_1".into()),
            name: "read".into(),
            arguments: r#"{"path":"a"}"#.into(),
            status: None,
        },
        ResponsesOutputItem::FunctionCall {
            id: None,
            call_id: CallId("call_2".into()),
            name: "read".into(),
            arguments: r#"{"path":"b"}"#.into(),
            status: None,
        },
    ])));
    assert_eq!(in_flight(&play), Some(("call_1".into(), "read".into())));
    play.tool_result("call_1", "…", false, WireState::Raw);
    assert_eq!(
        in_flight(&play),
        None,
        "the loop appends results only after every call ran, so the first result ends the step"
    );
}
