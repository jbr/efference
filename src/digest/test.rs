use super::*;
use crate::log::{AssistantOutput, Distilled, Keep, Mark, RawResult, Resumed, Timestamp};
use serde_json::json;

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(1_700_000_000_000 + seq),
        submission_id: None,
        source: None,
        payload,
    }
}

fn user(seq: u64, text: &str) -> LogEntry {
    entry(seq, Payload::User(vec![ContentBlock::Text(text.into())]))
}

fn assistant(seq: u64, text: &str, calls: Vec<ToolCall>) -> LogEntry {
    let text = if text.is_empty() {
        vec![]
    } else {
        vec![ContentBlock::Text(text.into())]
    };
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(vec![], text, calls),
            upstream_failure: None,
        },
    )
}

fn distilled(seq: u64, call: &str, raw: RawResult, note: &str) -> LogEntry {
    entry(
        seq,
        Payload::ToolResult {
            call_id: call.into(),
            raw,
            wire: WireState::Distilled(Distilled::new(Keep::All).with_note(note)),
        },
    )
}

fn read_call(id: &str) -> ToolCall {
    ToolCall::new("read", id, json!({"paths": ["src/a.rs", "src/b.rs"]}))
        .with_intent("find where the enum is defined")
}

#[test]
fn a_call_renders_its_paths_intent_and_note_without_bodies() {
    let log = [
        user(1, "find the enum"),
        assistant(2, "Looking.", vec![read_call("c1")]),
        distilled(
            3,
            "c1",
            RawResult::text("BODY ".repeat(500)),
            "Defined in a.rs.",
        ),
        assistant(4, "It is in a.rs.", vec![]),
    ];
    assert_eq!(
        render(&log, Options::default()),
        "### operator\nfind the enum\n\n\
         • [read src/a.rs, src/b.rs] find where the enum is defined\n    → Defined in a.rs.\n\
         ### agent\nIt is in a.rs.\n\n"
    );
}

#[test]
fn upto_cuts_and_seqs_label() {
    let log = [
        user(1, "first"),
        assistant(
            2,
            "",
            vec![ToolCall::new("bash", "c1", json!({"command": "ls"}))],
        ),
        distilled(3, "c1", RawResult::error("nope"), "ls failed"),
        user(4, "second"),
    ];
    let options = Options {
        upto: Some(3),
        seqs: true,
    };
    assert_eq!(
        render(&log, options),
        "### operator (seq 1)\nfirst\n\n2 • [bash]\n    → (error) ls failed\n"
    );
}

#[test]
fn a_verdict_resolves_a_pending_result_and_resumption_skips_are_omitted() {
    let log = [
        assistant(1, "", vec![read_call("c1")]),
        entry(
            2,
            Payload::ToolResult {
                call_id: "c1".into(),
                raw: RawResult::text("x"),
                wire: WireState::Pending,
            },
        ),
        entry(
            3,
            Payload::ResultVerdict {
                for_result: 2,
                wire: WireState::Distilled(Distilled::new(Keep::All).with_note("late note")),
            },
        ),
        user(4, "set aside"),
        entry(
            5,
            Payload::Resumed(Resumed {
                gap_ms: 1,
                lost_processes: vec![],
                resumes_after: Some(3),
                pending_calls: vec![],
            }),
        ),
        entry(
            6,
            Payload::Mark(Mark {
                kind: MarkKind::Boundary { upto: 3 },
                note: "summary".into(),
            }),
        ),
    ];
    assert_eq!(
        render(&log, Options::default()),
        "• [read src/a.rs, src/b.rs] find where the enum is defined\n    → late note\n\
         ### consolidated through 3\nsummary\n\n"
    );
}

fn ask_reply(seq: u64, call: &str, text: &str, answered: bool) -> LogEntry {
    let mut e = entry(
        seq,
        Payload::ToolResult {
            call_id: call.into(),
            raw: RawResult::text(text),
            wire: WireState::Raw,
        },
    );
    if answered {
        e.submission_id = Some("s1".into());
    }
    e
}

/// An ask is a question and its reply, attributed like a turn; one with no
/// submission stayed open and says so.
#[test]
fn asks_render_as_question_and_reply() {
    let ask = |id: &str, q: &str| ToolCall::new("ask", id, json!({"question": q}));
    let mut sourced = ask_reply(4, "a2", "use the second", true);
    sourced.source = Some("mcp[claude-code]".into());
    let log = [
        assistant(1, "Two options.", vec![ask("a1", "Which approach?")]),
        ask_reply(2, "a1", "the first", true),
        assistant(3, "", vec![ask("a2", "And then?")]),
        sourced,
        assistant(5, "", vec![ask("a3", "Ship it?")]),
        ask_reply(6, "a3", "interrupted", false),
    ];
    assert_eq!(
        render(&log, Options::default()),
        "### agent\nTwo options.\n\n### agent asked\nWhich approach?\n\n### operator replied\nthe first\n\n\
         ### agent asked\nAnd then?\n\n### mcp[claude-code] replied\nuse the second\n\n\
         ### agent asked\nShip it?\n\n(not answered)\n\n"
    );
}
