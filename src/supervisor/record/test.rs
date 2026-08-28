//! The log fold behind `GET /processes/:id` on a session with no daemon
//! (docs/supervisor.md).

use super::*;

use crate::log::{CallId, ExitOutcome, ProcessEventKind, ProcessSpawn, RawResult, Seq, WireState};

/// One log entry carrying `payload`, at `seq`.
fn entry(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(1_754_300_000_000 + seq),
        submission_id: None,
        payload,
    }
}

fn spawned(id: u64) -> Payload {
    Payload::ProcessEvent(ProcessEvent {
        id,
        kind: ProcessEventKind::Spawned(ProcessSpawn {
            command: format!("sleep {id}"),
            call_id: Some(CallId("toolu_1".into())),
            pid: Some(4242),
            at: Timestamp(1_754_300_000_000),
        }),
    })
}

fn exited(id: u64) -> Payload {
    Payload::ProcessEvent(ProcessEvent {
        id,
        kind: ProcessEventKind::Exited(ProcessExit {
            outcome: ExitOutcome::Code(1),
            duration_ms: 12_000,
            at: Timestamp(1_754_300_012_000),
        }),
    })
}

/// A log that is nothing but a transcript — the shape of every log written
/// before the record existed — answers `None` rather than erroring.
#[test]
fn a_log_with_no_process_records_answers_nothing() {
    let entries = vec![
        entry(
            0,
            Payload::User(vec![crate::log::ContentBlock::Text("go".into())]),
        ),
        entry(
            1,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    content: "backgrounded as p#1".into(),
                    is_error: false,
                    content_elided: None,
                    images: Vec::new(),
                },
                wire: WireState::Raw,
            },
        ),
    ];
    assert!(from_log(&entries, 1).is_none());
}

/// The mint alone answers the metadata question — command, spawning call, spawn
/// time — which is the promise the record makes even when the exit was never
/// observed (the daemon died first).
#[test]
fn a_mint_alone_answers_the_metadata() {
    let entries = vec![entry(0, spawned(3))];
    let record = from_log(&entries, 3).expect("the mint is the record");
    assert_eq!(record.id, 3);
    assert_eq!(record.command, "sleep 3");
    assert_eq!(record.call_id.as_deref(), Some("toolu_1"));
    assert_eq!(record.spawned_at, Timestamp(1_754_300_000_000));
    assert!(record.background);
    assert!(!record.running, "a log fold never claims a live process");
    assert!(
        record.exit.is_none(),
        "no exit was observed, so none is claimed"
    );
}

/// Mint + exit is the whole record, and the fold reads only the id it was asked
/// for — a session that backgrounded several commands keeps them apart.
#[test]
fn mint_and_exit_fold_to_one_record_per_id() {
    let entries = vec![
        entry(0, spawned(1)),
        entry(1, spawned(2)),
        entry(2, exited(2)),
    ];
    let one = from_log(&entries, 1).expect("p#1");
    assert!(one.exit.is_none(), "p#2's exit is not p#1's");

    let two = from_log(&entries, 2).expect("p#2");
    let exit = two.exit.expect("the exit was recorded");
    assert_eq!(exit.outcome, ExitOutcome::Code(1));
    assert_eq!(exit.duration_ms, 12_000);
    assert_eq!(exit.at, Timestamp(1_754_300_012_000));
    assert!(!two.running);
}

/// The record survives the log's own serialization — the round-trip a recorded
/// session actually goes through, since the fold reads entries parsed back off
/// disk.
#[test]
fn lifecycle_entries_round_trip_through_the_log_line() {
    let entries: Vec<LogEntry> = [spawned(7), exited(7)]
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            let line = serde_json::to_string(&entry(i as Seq, payload)).unwrap();
            serde_json::from_str(&line).expect("a lifecycle entry parses back")
        })
        .collect();
    let record = from_log(&entries, 7).expect("p#7");
    assert_eq!(record.command, "sleep 7");
    assert_eq!(record.pid, Some(4242));
    assert_eq!(record.exit.expect("exit").outcome, ExitOutcome::Code(1));
}
