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
        upstream_failure: None,
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
