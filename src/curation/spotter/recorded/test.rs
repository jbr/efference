use super::*;
use crate::config::testing::ConfigFixture;
use crate::record::log::{AssistantOutput, ConfigEdit, RawResult, ToolOutcome, ToolTelemetry};
use crate::record::log::{Stance, WireState};
use serde_json::json;

fn entry(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry::new(seq, payload).with_turn(1)
}

fn assistant(seq: Seq, calls: Vec<ToolCall>) -> LogEntry {
    entry(
        seq,
        Payload::assistant(AssistantOutput::from_legacy_messages(vec![], vec![], calls)),
    )
}

fn bash(id: &str, command: &str) -> ToolCall {
    ToolCall::new("bash", id, json!({ "command": command }))
}

/// The telemetry a call that ran leaves behind, which is what makes it consented.
fn ran(seq: Seq, id: &str) -> LogEntry {
    entry(
        seq,
        Payload::Telemetry(Telemetry::Tool(ToolTelemetry {
            call_id: id.into(),
            tool: "bash".into(),
            duration_ms: 1,
            outcome: ToolOutcome::Applied,
            result_bytes: 0,
            images: 0,
            bash: None,
            edit: None,
        })),
    )
}

fn granted(seq: Seq, change: ConfigChange) -> LogEntry {
    entry(
        seq,
        Payload::ConfigEdit(ConfigEdit::new(EditState::Committed, change)),
    )
}

fn path(key: &str) -> ConfigChange {
    ConfigChange::Path {
        path: key.into(),
        read: true,
        write: true,
    }
}

/// A session that ran a test, was granted `../other`, held a removal at seq 6 with a
/// probe, and then was granted `/later` and told something more.
fn session() -> Vec<LogEntry> {
    vec![
        entry(0, Payload::user("run the tests, then clean the target dir")),
        assistant(1, vec![bash("a", "cargo test")]),
        ran(2, "a"),
        entry(
            3,
            Payload::tool_result("a", RawResult::text("ok"), WireState::Raw),
        ),
        granted(4, path("../other")),
        assistant(5, vec![bash("b", "rm -rf target")]),
        entry(
            6,
            Payload::Spot(
                Spot::new("b", Stance::Gate, "a deletion").with_probed_prefix(Some("rm".into())),
            ),
        ),
        granted(7, path("/later")),
        entry(8, Payload::user("and the docs too")),
        ran(9, "b"),
        assistant(10, vec![bash("c", "ls"), bash("d", "cargo doc")]),
        entry(11, Payload::Telemetry(Telemetry::call_started("c"))),
    ]
}

fn no_held(_: &str) -> Option<Vec<LogEntry>> {
    None
}

fn texts(move_: &RecordedMove) -> Vec<&str> {
    move_
        .utterances
        .said
        .iter()
        .map(|u| u.text.as_str())
        .collect()
}

fn ids(calls: &[ToolCall]) -> Vec<&str> {
    calls.iter().map(|c| c.id.0.as_str()).collect()
}

/// A verdict's seq names the call it judged, and what the request read is the log
/// before the verdict: nothing said, ran, or granted after it.
#[test]
fn a_verdict_rebuilds_its_call_and_what_preceded_it() {
    let log = session();
    let found =
        RecordedMove::find("s", &log, Some(Handle::parse("6").unwrap()), None, no_held).unwrap();
    assert_eq!(found.judged, bash("b", "rm -rf target"));
    assert_eq!(found.call, found.judged);
    assert_eq!(found.cut, Some(6));
    assert_eq!(found.spot.as_ref().map(|(seq, _)| *seq), Some(6));
    assert_eq!(texts(&found), ["run the tests, then clean the target dir"]);
    // `b` ran too, but only after the verdict.
    assert_eq!(ids(&found.precedents), ["a"]);
    assert_eq!(found.probe(), Some("rm"));
    assert!(found.chain.is_empty());
    assert!(found.notes.is_empty(), "{:?}", found.notes);

    let circumstances = found.circumstances(None);
    assert_eq!(circumstances.probe, Some("rm"));
    assert_eq!(circumstances.utterances.map(|u| u.said.len()), Some(1));
    assert!(circumstances.supervision.is_none());
}

/// The call's id, and its assistant entry's handle, name the same verdict as its seq.
#[test]
fn a_call_id_or_handle_names_the_same_move() {
    let log = session();
    for found in [
        RecordedMove::find("s", &log, None, Some("b"), no_held).unwrap(),
        RecordedMove::find("s", &log, Some(Handle::parse("5").unwrap()), None, no_held).unwrap(),
        RecordedMove::find(
            "s",
            &log,
            Some(Handle::parse("6").unwrap()),
            Some("b"),
            no_held,
        )
        .unwrap(),
    ] {
        assert_eq!(found.judged.id.0, "b");
        assert_eq!(found.cut, Some(6));
        assert_eq!(texts(&found).len(), 1);
    }
    let disagreeing = RecordedMove::find(
        "s",
        &log,
        Some(Handle::parse("6").unwrap()),
        Some("a"),
        no_held,
    );
    assert!(disagreeing.is_err());
}

/// A call never spotted is shown as it stood before its first later entry: here its
/// start, after everything said and ran before it.
#[test]
fn a_call_never_spotted_reads_the_log_up_to_its_start() {
    let log = session();
    let found = RecordedMove::find(
        "s",
        &log,
        Some(Handle::parse("10.1").unwrap()),
        None,
        no_held,
    )
    .unwrap();
    assert_eq!(found.judged.id.0, "c");
    assert!(found.spot.is_none());
    assert_eq!(found.cut, Some(11));
    assert_eq!(
        texts(&found),
        [
            "run the tests, then clean the target dir",
            "and the docs too"
        ]
    );
    assert_eq!(ids(&found.precedents), ["a", "b"]);
    assert_eq!(found.probe(), None);

    // The second call of the same entry has no later entry at all: the whole log.
    let last = RecordedMove::find(
        "s",
        &log,
        Some(Handle::parse("10.2").unwrap()),
        None,
        no_held,
    )
    .unwrap();
    assert_eq!(last.judged.id.0, "d");
    assert_eq!(last.cut, None);
}

/// The file holds every grant, the later one too; the rebuilt config takes the later
/// one back out, keeps the earlier one, and says what it cannot know.
#[test]
fn the_config_leaves_out_what_was_granted_after_the_move() {
    let log = session();
    let file = ConfigFixture::new()
        .with_path(".", true, true)
        .with_path("../other", true, true)
        .with_path("/later", true, true)
        .with_scope("work here")
        .build();
    let found =
        RecordedMove::find("s", &log, Some(Handle::parse("6").unwrap()), None, no_held).unwrap();
    let (role, notes) = found.config(&file);
    let keys: Vec<&str> = role.paths.keys().map(String::as_str).collect();
    assert_eq!(keys, [".", "../other"]);
    assert_eq!(role.scope.as_deref(), Some("work here"));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("`/later`"), "{notes:?}");

    // At the later call, the grant had been made.
    let later = RecordedMove::find("s", &log, None, Some("c"), no_held).unwrap();
    let (role, notes) = later.config(&file);
    assert!(role.paths.contains_key("/later"));
    assert!(notes.is_empty(), "{notes:?}");
}

/// An allowing decision is judged as the leaf it authorizes, read from the held
/// session's log, under the decision's own id and with no precedents of its own.
#[test]
fn an_allowing_decision_is_judged_as_its_leaf() {
    let decide = ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
        "p1",
        json!({ "agent": "child", "seq": "3", "decision": "allow" }),
    );
    let log = vec![
        entry(0, Payload::user("let the child push")),
        assistant(1, vec![bash("a", "ls")]),
        ran(2, "a"),
        assistant(3, vec![decide.clone()]),
        entry(4, Payload::Spot(Spot::new("p1", Stance::Watch, "a push"))),
    ];
    let child = vec![assistant(3, vec![bash("x", "git push")])];
    let held = |id: &str| (id == "child").then(|| child.clone());
    let found =
        RecordedMove::find("s", &log, Some(Handle::parse("4").unwrap()), None, held).unwrap();
    assert_eq!(found.call, decide);
    assert_eq!(found.judged, bash("p1", "git push"));
    assert_eq!(found.chain, ["child"]);
    assert!(found.precedents.is_empty());
    assert_eq!(texts(&found), ["let the child push"]);

    // Without the child's log, the decision itself stands in, and the notes say so.
    let unread =
        RecordedMove::find("s", &log, Some(Handle::parse("4").unwrap()), None, no_held).unwrap();
    assert_eq!(unread.judged, decide);
    assert!(unread.chain.is_empty());
    assert_eq!(unread.notes.len(), 1, "{:?}", unread.notes);
}
