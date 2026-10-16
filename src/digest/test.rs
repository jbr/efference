use super::*;
use crate::log::{AssistantOutput, Keep, RawResult, Resumed, Summary, Timestamp, Triaged};
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

fn triaged(seq: u64, call: &str, raw: RawResult, note: &str) -> LogEntry {
    entry(
        seq,
        Payload::ToolResult {
            call_id: call.into(),
            raw,
            wire: WireState::Triaged(Triaged::new(Keep::All).with_note(note)),
        },
    )
}

fn read_call(id: &str) -> ToolCall {
    ToolCall::new("read", id, json!({"paths": ["src/a.rs", "src/b.rs"]}))
        .with_intent("find where the enum is defined")
}

/// Mid-turn prose renders too ("Looking." precedes a call): it is where the agent says
/// what it is about to do (docs/digest.md).
#[test]
fn a_call_renders_its_paths_intent_and_note_without_bodies() {
    let log = [
        user(1, "find the enum"),
        assistant(2, "Looking.", vec![read_call("c1")]),
        triaged(
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
         ### agent\nLooking.\n\n\
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
        triaged(3, "c1", RawResult::error("nope"), "ls failed"),
        user(4, "second"),
    ];
    let options = Options {
        upto: Some(3),
        seqs: true,
        ..Options::default()
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
                wire: WireState::Triaged(Triaged::new(Keep::All).with_note("late note")),
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
        entry(6, Payload::Summary(Summary::consolidation(0, 3, "summary"))),
    ];
    assert_eq!(
        render(&log, Options::default()),
        "• [read src/a.rs, src/b.rs] find where the enum is defined\n    → late note\n\
         ### consolidated through 3\nsummary\n\n"
    );
}

/// A summary addressed to the triage reader renders in a digest read for
/// triage and is left out of one read for the main model (docs/summary.md).
#[test]
fn a_triage_mark_renders_only_in_a_triage_digest() {
    use crate::log::Audience;
    use crate::project::Reader;
    let log = vec![
        entry(0, Payload::User(vec![ContentBlock::Text("first".into())])),
        entry(
            1,
            Payload::Summary(
                Summary::consolidation(0, 0, "for triage").for_audience(Audience::Triage),
            ),
        ),
    ];
    let main = render(&log, Options::default());
    assert!(!main.contains("for triage"), "{main}");
    let triage = render(
        &log,
        Options {
            reader: Reader::Triage,
            ..Options::default()
        },
    );
    assert!(
        triage.contains("### consolidated through 0\nfor triage"),
        "{triage}"
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

#[test]
fn the_cut_marks_the_first_rendered_block_past_it() {
    let log = [
        user(1, "find the enum"),
        assistant(2, "", vec![read_call("c1")]),
        triaged(3, "c1", RawResult::text("x"), "Defined in a.rs."),
        // Past the cut, but renders nothing: the marker waits for the next block.
        entry(
            4,
            Payload::ResultVerdict {
                for_result: 3,
                wire: WireState::Triaged(Triaged::new(Keep::All).with_note("Defined in a.rs.")),
            },
        ),
        assistant(5, "It is in a.rs.", vec![]),
    ];
    let options = Options {
        cut: Some(3),
        ..Default::default()
    };
    assert_eq!(
        render(&log, options),
        format!(
            "### operator\nfind the enum\n\n\
             • [read src/a.rs, src/b.rs] find where the enum is defined\n    → Defined in a.rs.\n\
             {CUT}### agent\nIt is in a.rs.\n\n"
        )
    );
    // A cut with nothing after it places no marker.
    let options = Options {
        cut: Some(5),
        ..Default::default()
    };
    assert!(!render(&log, options).contains(CUT));
}

/// The digest at one triage extends the digest at every earlier one, byte for byte:
/// the triage's history ends at the assistant turn that made its calls, and every earlier
/// result has its verdict by then. That is what lets a triage request that reads the
/// digest resume a cached prefix (backlog: separate-triage-reads-text.md).
#[test]
fn the_digest_at_a_gate_extends_the_digest_at_every_earlier_gate() {
    let log = [
        user(1, "find the enum"),
        assistant(2, "Looking.", vec![read_call("c1")]),
        triaged(3, "c1", RawResult::text("a"), "Defined in a.rs."),
        assistant(4, "Now its uses.", vec![read_call("c2")]),
        triaged(5, "c2", RawResult::text("b"), "Used twice."),
        assistant(6, "", vec![read_call("c3")]),
        triaged(7, "c3", RawResult::text("c"), "Nothing else."),
        assistant(8, "Done: it is in a.rs.", vec![]),
        user(9, "thanks"),
        assistant(10, "One more look.", vec![read_call("c4")]),
    ];
    // Each triage's history: through an assistant turn that made calls.
    let triages: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(&e.payload, Payload::Assistant { output, .. }
                if output.tool_calls().is_ok_and(|c| !c.is_empty()))
        })
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(triages.len(), 4);
    let digests: Vec<String> = triages
        .iter()
        .map(|&end| render(&log[..end], Options::default()))
        .collect();
    for pair in digests.windows(2) {
        assert!(
            pair[1].starts_with(&pair[0]),
            "a later triage's digest rewrote an earlier one's:\n{}\n---\n{}",
            pair[0],
            pair[1]
        );
    }
}
