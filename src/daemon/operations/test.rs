//! Tests for `operations.rs`: the fold behind `current_operations`.

use super::*;
use crate::log::Telemetry;
use crate::log::{AssistantOutput, ContentBlock, RawResult, Resumed, ToolCall, WireState};
use crate::persist::Log;

fn entry(seq: u64, at: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(at),
        submission_id: None,
        source: None,
        payload,
    }
}

fn stage(request: u64, origin: RequestOrigin, stage: RequestStageKind) -> Payload {
    Payload::Telemetry(Telemetry::RequestStage(RequestStage::new(
        request, origin, stage,
    )))
}

fn assistant_calling(call: ToolCall) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(vec![], vec![], vec![call]),
        excluded: None,
    }
}

fn resumed() -> Payload {
    Payload::Resumed(Resumed {
        gap_ms: 5,
        lost_processes: Vec::new(),
        resumes_after: None,
        pending_calls: Vec::new(),
        tools_changed: Default::default(),
    })
}

fn fold(payloads: Vec<(u64, Payload)>) -> Operations {
    let entries: Vec<LogEntry> = payloads
        .into_iter()
        .enumerate()
        .map(|(seq, (at, payload))| entry(seq as u64, at, payload))
        .collect();
    Operations::fold(&entries)
}

/// Nothing open is `null`, not an empty object (docs/control-api.md).
#[test]
fn nothing_open_is_null() {
    assert_eq!(Operations::default().json(), Value::Null);
}

/// A request is open from `sent` to its closing stage, and each stage restarts
/// `since` at that stage's own time.
#[test]
fn a_request_is_open_until_it_closes() {
    use RequestOrigin::Turn;
    let sent = fold(vec![(100, stage(0, Turn, RequestStageKind::Sent))]);
    assert_eq!(
        sent.json(),
        json!({"requests": [{"request": 0, "origin": "turn", "stage": "sent", "since": 100}],
               "tools": [], "compaction": null})
    );

    let streaming = fold(vec![
        (100, stage(0, Turn, RequestStageKind::Sent)),
        (250, stage(0, Turn, RequestStageKind::FirstByte)),
    ]);
    assert_eq!(streaming.json()["requests"][0]["stage"], "streaming");
    assert_eq!(streaming.json()["requests"][0]["since"], 250);

    let retrying = fold(vec![
        (100, stage(0, Turn, RequestStageKind::Sent)),
        (
            200,
            stage(
                0,
                Turn,
                RequestStageKind::AttemptFailed {
                    attempt: 2,
                    status: 429,
                },
            ),
        ),
    ]);
    let request = &retrying.json()["requests"][0];
    assert_eq!(request["stage"], "retrying");
    assert_eq!(request["attempt"], 2);
    assert_eq!(request["status"], 429);

    for closing in [
        RequestStageKind::Ended,
        RequestStageKind::Failed {
            reason: "interrupted".into(),
        },
    ] {
        let closed = fold(vec![
            (100, stage(0, Turn, RequestStageKind::Sent)),
            (300, stage(0, Turn, closing)),
        ]);
        assert_eq!(closed.json(), Value::Null);
    }
}

/// A triage and the spotter open at once are both listed, and closing one leaves the
/// other.
#[test]
fn concurrent_requests_are_listed_separately() {
    let ops = fold(vec![
        (10, stage(3, RequestOrigin::Triage, RequestStageKind::Sent)),
        (20, stage(4, RequestOrigin::Spot, RequestStageKind::Sent)),
        (30, stage(3, RequestOrigin::Triage, RequestStageKind::Ended)),
    ]);
    let json = ops.json();
    let requests = json["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["origin"], "spot");
    assert_eq!(requests[0]["request"], 4);
}

/// A stage for a request that is not open does not open it: the entry belongs to a
/// request the log already closed or a restart dropped.
#[test]
fn a_stage_without_a_sent_opens_nothing() {
    let ops = fold(vec![(
        10,
        stage(9, RequestOrigin::Turn, RequestStageKind::FirstByte),
    )]);
    assert_eq!(ops.json(), Value::Null);
}

/// A restart closes what the previous host process left open, and ids keep
/// counting past every one the log used.
#[test]
fn a_resumed_entry_closes_everything_before_it() {
    let mut ops = fold(vec![
        (10, stage(5, RequestOrigin::Turn, RequestStageKind::Sent)),
        (
            20,
            assistant_calling(ToolCall::new("bash", "t1", json!({"command": "ls"}))),
        ),
        (
            30,
            Payload::Telemetry(Telemetry::CallStarted {
                call_id: "t1".into(),
            }),
        ),
        (40, resumed()),
    ]);
    assert_eq!(ops.json(), Value::Null);
    assert_eq!(ops.mint(), 6);

    // The same request id sent again after the restart opens normally.
    ops.observe(
        Timestamp(50),
        &stage(6, RequestOrigin::Turn, RequestStageKind::Sent),
    );
    assert_eq!(ops.json()["requests"][0]["request"], 6);
}

/// A call is running from `CallStarted` to its result, named from the assistant
/// entry that issued it, with its intent.
#[test]
fn a_started_call_is_running_until_its_result() {
    let call =
        ToolCall::new("bash", "t1", json!({"command": "cargo test"})).with_intent("run the suite");
    let started = vec![
        (10, assistant_calling(call)),
        (
            20,
            Payload::Telemetry(Telemetry::CallStarted {
                call_id: "t1".into(),
            }),
        ),
    ];
    let ops = fold(started.clone());
    assert_eq!(
        ops.json()["tools"],
        json!([{"call_id": "t1", "tool": "bash", "intent": "run the suite",
                "command": "cargo test", "since": 20}])
    );

    let mut finished = started;
    finished.push((
        90,
        Payload::tool_result("t1", RawResult::text("ok"), WireState::Raw),
    ));
    assert_eq!(fold(finished).json(), Value::Null);
}

/// Only `bash` carries a `command`; another tool's input is not echoed.
#[test]
fn only_a_bash_call_carries_its_command() {
    let ops = fold(vec![
        (
            10,
            assistant_calling(ToolCall::new("read", "t1", json!({"command": "x"}))),
        ),
        (
            20,
            Payload::Telemetry(Telemetry::CallStarted {
                call_id: "t1".into(),
            }),
        ),
    ]);
    assert!(ops.json()["tools"][0].get("command").is_none());
}

/// A call approved but not yet started (waiting at a gate) is not running.
#[test]
fn an_issued_call_is_not_running_before_it_starts() {
    let ops = fold(vec![(
        10,
        assistant_calling(ToolCall::new("read", "t1", json!({"path": "a"}))),
    )]);
    assert_eq!(ops.json(), Value::Null);
}

/// A user turn that interrupts a batch drops its running calls.
#[test]
fn a_user_entry_clears_running_calls() {
    let ops = fold(vec![
        (
            10,
            assistant_calling(ToolCall::new("bash", "t1", json!({"command": "sleep 99"}))),
        ),
        (
            20,
            Payload::Telemetry(Telemetry::CallStarted {
                call_id: "t1".into(),
            }),
        ),
        (30, Payload::User(vec![ContentBlock::Text("stop".into())])),
    ]);
    assert_eq!(ops.json(), Value::Null);
}

/// The session maintains the view as entries append, and the sink mints ids that
/// stay unique, appends the stages, and reports through `GET /state`.
#[test]
fn the_session_view_follows_appends() {
    let (session, _turns) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let state = |session: &Session| -> Value {
        serde_json::from_str::<Value>(&session.state_json()).unwrap()["current_operations"].clone()
    };
    assert_eq!(state(&session), Value::Null);

    let sink = session.stage_sink();
    let a = sink.mint();
    let b = sink.mint();
    assert_ne!(a, b);
    sink.record(RequestStage::new(
        a,
        RequestOrigin::Turn,
        RequestStageKind::Sent,
    ));
    let open = state(&session);
    assert_eq!(open["requests"][0]["request"], a);
    assert_eq!(open["requests"][0]["stage"], "sent");
    let logged = session.snapshot();
    assert!(matches!(
        &logged.last().unwrap().payload,
        Payload::Telemetry(Telemetry::RequestStage(s)) if s.request == a
    ));

    sink.record(RequestStage::new(
        a,
        RequestOrigin::Turn,
        RequestStageKind::Ended,
    ));
    assert_eq!(state(&session), Value::Null);
}

/// A session opened from a log that ends mid-request reads the same view the
/// running one had, except that a `Resumed` entry closes it.
#[test]
fn a_session_opened_from_a_log_folds_it() {
    let mut log = Log::in_memory();
    log.append(1, stage(0, RequestOrigin::Turn, RequestStageKind::Sent))
        .unwrap();
    let (session, _turns) = Session::new(std::env::current_dir().unwrap(), log);
    let state = |session: &Session| -> Value {
        serde_json::from_str::<Value>(&session.state_json()).unwrap()["current_operations"].clone()
    };
    assert_eq!(state(&session)["requests"][0]["request"], 0);
    // New ids continue past the log's.
    assert_eq!(session.stage_sink().mint(), 1);

    session.append(1, resumed()).unwrap();
    assert_eq!(state(&session), Value::Null);
}

fn compaction_requested(cut: u64) -> Payload {
    Payload::Telemetry(Telemetry::CompactionRequested { warm: true, cut })
}

fn compaction_failed(error: &str) -> Payload {
    Payload::Telemetry(Telemetry::CompactionFailed {
        error: error.into(),
    })
}

fn compaction_summary(upto: u64) -> Payload {
    let model = crate::log::LoggedInference::new("c", crate::api::Provider::Anthropic, "m");
    Payload::Summary(Summary::compaction(upto, "account", model))
}

/// A compaction waits from its request until its summarizing request is sent, writes
/// until its summary lands, and is closed by that summary (docs/summary.md).
#[test]
fn a_compaction_waits_then_writes_then_closes() {
    use RequestOrigin::Compaction;
    let waiting = fold(vec![(100, compaction_requested(4))]);
    assert_eq!(
        waiting.compaction(),
        Some(&CompactionState::Waiting {
            warm: true,
            cut: 4,
            since: 100
        })
    );

    let writing = fold(vec![
        (100, compaction_requested(4)),
        (150, stage(0, Compaction, RequestStageKind::Sent)),
    ]);
    assert_eq!(
        writing.compaction(),
        Some(&CompactionState::Writing {
            warm: true,
            cut: 4,
            since: 150
        })
    );

    let landed = fold(vec![
        (100, compaction_requested(4)),
        (150, stage(0, Compaction, RequestStageKind::Sent)),
        (200, stage(0, Compaction, RequestStageKind::Ended)),
        (210, compaction_summary(4)),
    ]);
    assert_eq!(landed.compaction(), None);
}

/// An open compaction is one of `current_operations`, beside its own request; nothing
/// else open still makes the topic non-null. A failure is not in flight, so it leaves
/// the topic (docs/control-api.md).
#[test]
fn an_open_compaction_is_an_operation_and_a_failure_is_not() {
    use RequestOrigin::Compaction;
    assert_eq!(
        fold(vec![(100, compaction_requested(4))]).json(),
        json!({"requests": [], "tools": [],
               "compaction": {"state": "waiting", "warm": true, "cut": 4, "since": 100}})
    );
    let writing = fold(vec![
        (100, compaction_requested(4)),
        (150, stage(0, Compaction, RequestStageKind::Sent)),
    ])
    .json();
    assert_eq!(
        writing["compaction"],
        json!({"state": "writing", "warm": true, "cut": 4, "since": 150})
    );
    assert_eq!(writing["requests"][0]["origin"], "compaction");
    assert_eq!(
        fold(vec![
            (100, compaction_requested(4)),
            (150, compaction_failed("boom")),
        ])
        .json(),
        Value::Null
    );
}

/// The agent's own summary is not the compaction's, and does not close it.
#[test]
fn a_consolidation_does_not_close_a_compaction() {
    let ops = fold(vec![
        (100, compaction_requested(4)),
        (120, Payload::Summary(Summary::consolidation(2, 3, "span"))),
    ]);
    assert!(ops.compaction().is_some_and(CompactionState::open));
}

/// A failure stands until a turn's request goes out or another compaction is asked
/// for; a triage or title request leaves it.
#[test]
fn a_failure_stands_until_a_turn_request_or_another_compaction() {
    let failed = vec![
        (100, compaction_requested(4)),
        (150, compaction_failed("boom")),
        (160, stage(0, RequestOrigin::Triage, RequestStageKind::Sent)),
    ];
    assert_eq!(
        fold(failed.clone()).compaction(),
        Some(&CompactionState::Failed {
            error: "boom".into(),
            at: 150
        })
    );

    let mut turned = failed.clone();
    turned.push((170, stage(1, RequestOrigin::Turn, RequestStageKind::Sent)));
    assert_eq!(fold(turned).compaction(), None);

    let mut again = failed;
    again.push((170, compaction_requested(5)));
    assert!(
        matches!(
            fold(again).compaction(),
            Some(CompactionState::Waiting { cut: 5, .. })
        ),
        "a new compaction replaces the failure"
    );
}

/// A failure is in the log, so it survives a restart; a compaction open when the host
/// stopped does not, since the process writing it is gone.
#[test]
fn a_failure_survives_a_restart_and_an_open_compaction_does_not() {
    let failed = fold(vec![
        (100, compaction_requested(4)),
        (150, compaction_failed("boom")),
        (200, resumed()),
    ]);
    assert!(matches!(
        failed.compaction(),
        Some(CompactionState::Failed { .. })
    ));

    let open = fold(vec![
        (100, compaction_requested(4)),
        (
            150,
            stage(0, RequestOrigin::Compaction, RequestStageKind::Sent),
        ),
        (200, resumed()),
    ]);
    assert_eq!(open.compaction(), None);
}
