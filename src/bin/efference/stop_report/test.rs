//! The fold over classified stops: the counts a preamble change is judged against,
//! and the run length that tells a stalled session from a few scattered pauses.

use super::*;
use efference::log::{AssistantOutput, ContentBlock, Payload, Timestamp};

fn entry(seq: u64, source: Option<&str>, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: seq as u32,
        at: Timestamp(0),
        submission_id: None,
        source: source.map(str::to_string),
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
        excluded: None,
    }
}

fn replied(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.to_string())])
}

/// A run is per session and resets on any other ending, so two sessions of two
/// barren stops each never read as a run of four.
#[test]
fn the_longest_run_is_within_one_session() {
    let stall = |id: &str| {
        let mut entries = Vec::new();
        for (i, reply) in ["ok", "👍"].iter().enumerate() {
            entries.push(entry(i as u64 * 2, None, said("Continuing.")));
            entries.push(entry(i as u64 * 2 + 1, None, replied(reply)));
        }
        (id.to_string(), entries)
    };
    let report = fold(&[stall("one"), stall("two")]);
    assert_eq!(report.barren, 4);
    assert_eq!(report.longest_run, 2);
}

/// An answered stop breaks the run, which is what separates a session that paused
/// usefully once from one that kept handing the turn back.
#[test]
fn an_answered_stop_breaks_the_run() {
    let entries = vec![
        entry(0, None, said("Continuing.")),
        entry(1, None, replied("ok")),
        entry(2, None, said("Continuing.")),
        entry(3, None, replied("now do the other one")),
        entry(4, None, said("Continuing.")),
        entry(5, None, replied("👍")),
    ];
    let report = fold(&[("one".to_string(), entries)]);
    assert_eq!((report.barren, report.answered), (2, 1));
    assert_eq!(report.longest_run, 1);
}

/// A nudge from a labelled client is counted apart: that is a supervising agent
/// spending its own context window to say "continue".
#[test]
fn a_nudge_from_an_agent_is_counted_apart() {
    let entries = vec![
        entry(0, None, said("Continuing.")),
        entry(1, Some("agent[pale-quail]"), replied("Continue.")),
        entry(2, None, said("Continuing.")),
        entry(3, None, replied("👍")),
    ];
    let report = fold(&[("one".to_string(), entries)]);
    assert_eq!(report.barren, 2);
    assert_eq!(report.barren_from_agents, 1);
}

/// Sessions are counted by whether they held a barren stop at all, so one long
/// stall does not read as a fleet-wide problem.
#[test]
fn sessions_are_counted_by_whether_they_stalled_at_all() {
    let clean = (
        "clean".to_string(),
        vec![
            entry(0, None, said("Should I commit here?")),
            entry(1, None, replied("yes")),
        ],
    );
    let stalled = (
        "stalled".to_string(),
        vec![
            entry(0, None, said("Continuing.")),
            entry(1, None, replied("ok")),
        ],
    );
    let report = fold(&[clean, stalled]);
    assert_eq!(
        (report.sessions_with_barren, report.sessions_with_stops),
        (1, 2)
    );
    assert_eq!((report.question, report.barren), (1, 1));
}

/// Asks are folded beside the turn endings without moving them: a log with only turn
/// endings has no ask counts, and a log with asks keeps its ending counts.
#[test]
fn asks_are_counted_beside_endings_and_leave_them_alone() {
    use efference::log::{RawResult, ToolCall, WireState};
    let ask = |id: &str| Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            Vec::new(),
            Vec::new(),
            vec![ToolCall::new(
                efference::process_monitor::ASK,
                id,
                serde_json::json!({ "question": "OK?" }),
            )],
        ),
        excluded: None,
    };
    let result =
        |id: &str, text: &str| Payload::tool_result(id, RawResult::text(text), WireState::Raw);
    let plain = ("plain".to_string(), vec![entry(0, None, said("Done."))]);
    let before = fold(std::slice::from_ref(&plain));
    assert_eq!(before.asks(), 0);

    let asking = (
        "asking".to_string(),
        vec![
            entry(0, None, ask("a1")),
            entry(1, None, result("a1", "sounds good")),
            entry(3, None, ask("a2")),
            entry(4, Some("agent[x]"), result("a2", "ok")),
            entry(5, None, ask("a3")),
            entry(6, None, result("a3", "use main")),
            entry(7, None, said("Done.")),
        ],
    );
    let after = fold(&[plain, asking]);
    assert_eq!((after.asks_barren, after.asks_answered), (2, 1));
    assert_eq!(after.asks_barren_from_agents, 1);
    assert_eq!(after.final_stop, before.final_stop + 1, "endings unchanged");
    assert_eq!(after.stops(), 2);
}
