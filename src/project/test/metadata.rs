//! Log metadata — per-call telemetry, process events, request stages, push and cold
//! records, gate decisions — projects to nothing: inserted anywhere, none moves a
//! projected byte, which is what lets each be appended mid-session without moving the
//! cached prefix (docs/log-and-projection.md).

use super::*;

/// Per-call tool telemetry (docs/tools.md) is log metadata in the same sense
/// timestamps are: a log carrying one entry per tool call projects
/// **byte-identically** to the same log without them. This is the pin that lets
/// the record be written unconditionally on every call — the moment it could
/// move a byte, it would be moving the cached prefix of every session that runs
/// a tool (docs/log-and-projection.md). Same shape as the `Triage.thinking` pin.
#[test]
fn tool_telemetry_projects_to_identical_bytes() {
    use crate::log::{BashTelemetry, EditTelemetry, ToolOutcome, ToolTelemetry};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    // Interleaved as the loop appends it (after the result), and again at the
    // tail — position must not matter either.
    for at in [2, 3] {
        let mut with = sample_log();
        with.insert(
            at,
            entry(
                99,
                1,
                Payload::Telemetry(Telemetry::Tool(ToolTelemetry {
                    call_id: CallId("toolu_1".into()),
                    tool: "bash".into(),
                    duration_ms: 1234,
                    outcome: ToolOutcome::Rejected,
                    result_bytes: 8192,
                    images: 2,
                    bash: Some(BashTelemetry {
                        exit_status: Some("1".into()),
                        background: false,
                        handle: Some("p#1".into()),
                    }),
                    edit: Some(EditTelemetry {
                        operation: Some("replace".into()),
                        anchor_bytes: Some(40),
                        content_bytes: Some(900),
                    }),
                })),
            ),
        );
        assert_eq!(
            without,
            serde_json::to_string(&project(&with)).unwrap(),
            "telemetry appended at index {at} must not perturb the projected wire"
        );
    }
}

/// A process lifecycle record (docs/process-monitor.md) is log metadata too: the mint
/// and the exit are appended mid-session, so if either moved a projected byte it
/// would invalidate the cached prefix of every session that backgrounds a command
/// (docs/log-and-projection.md).
#[test]
fn process_events_project_to_identical_bytes() {
    use crate::log::{ExitOutcome, ProcessEvent, ProcessEventKind, ProcessExit, ProcessSpawn};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let events = [
        ProcessEventKind::Spawned(ProcessSpawn {
            command: "cargo watch -x test".into(),
            call_id: Some(CallId("toolu_1".into())),
            pid: Some(4242),
            at: Timestamp(1_754_300_000_000),
        }),
        ProcessEventKind::Exited(ProcessExit {
            outcome: ExitOutcome::Code(1),
            duration_ms: 12_000,
            at: Timestamp(1_754_300_012_000),
        }),
    ];
    // Interleaved mid-log and at the tail — position must not matter either.
    for at in [2, 3] {
        for kind in events.clone() {
            let mut with = sample_log();
            with.insert(
                at,
                entry(99, 1, Payload::ProcessEvent(ProcessEvent { id: 3, kind })),
            );
            assert_eq!(
                without,
                serde_json::to_string(&project(&with)).unwrap(),
                "a process record appended at index {at} must not perturb the projected wire"
            );
        }
    }
}

/// A request stage (docs/log-and-projection.md) is telemetry: a request's stages are
/// appended mid-turn, concurrently with other requests', so if any moved a projected
/// byte it would invalidate the cached prefix on every request.
#[test]
fn request_stages_project_to_identical_bytes() {
    use crate::log::{RequestOrigin, RequestStage, RequestStageKind};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let without_semantic = format!("{:?}", project_semantic(&sample_log()));
    let stages = [
        RequestStageKind::Sent,
        RequestStageKind::AttemptFailed {
            attempt: 1,
            status: 503,
        },
        RequestStageKind::FirstByte,
        RequestStageKind::Failed {
            reason: "interrupted".into(),
        },
        RequestStageKind::Ended,
    ];
    for at in [0, 1, 2, 3] {
        let mut with = sample_log();
        for (i, stage) in stages.iter().cloned().enumerate() {
            with.insert(
                at,
                entry(
                    90 + i as u64,
                    1,
                    Payload::Telemetry(Telemetry::RequestStage(RequestStage::new(
                        7,
                        RequestOrigin::Turn,
                        stage,
                    ))),
                ),
            );
        }
        assert_eq!(
            without,
            serde_json::to_string(&project(&with)).unwrap(),
            "request stages inserted at index {at} must not perturb the projected wire"
        );
        assert_eq!(
            without_semantic,
            format!("{:?}", project_semantic(&with)),
            "nor the dialect-neutral fold"
        );
    }
}

/// Push records (docs/web-push.md) are notification bookkeeping: inserted anywhere,
/// they change no projected byte.
#[test]
fn push_records_project_to_identical_bytes() {
    use crate::log::{
        DeliveryOutcome, DeviceDelivery, MomentKind, PushCancel, PushEvent, PushRecord,
    };
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let without_semantic = format!("{:?}", project_semantic(&sample_log()));
    let events = [
        PushEvent::Sent,
        PushEvent::Held {
            delay_secs: 120,
            attentive: vec![],
        },
        PushEvent::Escalated,
        PushEvent::Cancelled {
            cause: PushCancel::Superseded,
        },
        PushEvent::Delivery {
            devices: vec![DeviceDelivery {
                host: "push.example.net".into(),
                device: "0badc0de".into(),
                outcome: DeliveryOutcome::Gone { status: 410 },
            }],
        },
    ];
    for at in [0, 1, 2, 3] {
        let mut with = sample_log();
        for (i, event) in events.iter().cloned().enumerate() {
            with.insert(
                at,
                entry(
                    90 + i as u64,
                    1,
                    Payload::Telemetry(Telemetry::Push(PushRecord {
                        moment: 1,
                        kind: MomentKind::Gate,
                        event,
                    })),
                ),
            );
        }
        assert_eq!(
            without,
            serde_json::to_string(&project(&with)).unwrap(),
            "push records inserted at index {at} must not perturb the projected wire"
        );
        assert_eq!(
            without_semantic,
            format!("{:?}", project_semantic(&with)),
            "nor the dialect-neutral fold"
        );
    }
}

/// Cold-check records (docs/control-api.md) are bookkeeping about the park and the
/// person actions that release it: inserted anywhere, they change no projected byte.
#[test]
fn cold_records_project_to_identical_bytes() {
    use crate::log::{ColdEvent, ColdReason, ReleasedBy, Timestamp, UnparkCause};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let without_semantic = format!("{:?}", project_semantic(&sample_log()));
    let events = [
        ColdEvent::Parked {
            reason: ColdReason::Expired,
            since: Timestamp(1_754_300_000_000),
            context_tokens: Some(120_000),
        },
        ColdEvent::Released {
            by: ReleasedBy::Approval,
        },
        ColdEvent::Unparked {
            cause: UnparkCause::Person,
        },
    ];
    for at in [0, 1, 2, 3] {
        let mut with = sample_log();
        for (i, event) in events.iter().cloned().enumerate() {
            with.insert(
                at,
                entry(90 + i as u64, 1, Payload::Telemetry(Telemetry::Cold(event))),
            );
        }
        assert_eq!(
            without,
            serde_json::to_string(&project(&with)).unwrap(),
            "cold records inserted at index {at} must not perturb the projected wire"
        );
        assert_eq!(
            without_semantic,
            format!("{:?}", project_semantic(&with)),
            "nor the dialect-neutral fold"
        );
    }
}

/// A gate's decision record (docs/control-api.md) is telemetry appended while the loop
/// waits at the gate, mid-turn: inserted anywhere, every decision shape changes no
/// projected byte.
#[test]
fn gate_decisions_project_to_identical_bytes() {
    use crate::frontend::Approval;
    use crate::log::{ConfigChange, GateDecision};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let without_semantic = format!("{:?}", project_semantic(&sample_log()));
    let decisions = [
        GateDecision::new("toolu_1", Approval::Approve, None),
        GateDecision::new("toolu_1", Approval::Abort, Some("web".into())),
        GateDecision::new(
            "toolu_1",
            Approval::Redirect("use trash instead".into()),
            Some("session[parent]".into()),
        ),
        GateDecision::new(
            "toolu_1",
            Approval::ConfigEdit {
                change: ConfigChange::Command {
                    command: "grep -n".into(),
                },
                note: "read-only".into(),
            },
            Some("mcp[claude-code]".into()),
        ),
    ];
    for at in [0, 1, 2, 3] {
        let mut with = sample_log();
        for (i, decision) in decisions.iter().cloned().enumerate() {
            with.insert(
                at,
                entry(
                    90 + i as u64,
                    1,
                    Payload::Telemetry(Telemetry::GateDecision(decision)),
                ),
            );
        }
        assert_eq!(
            without,
            serde_json::to_string(&project(&with)).unwrap(),
            "decision records inserted at index {at} must not perturb the projected wire"
        );
        assert_eq!(
            without_semantic,
            format!("{:?}", project_semantic(&with)),
            "nor the dialect-neutral fold"
        );
    }
}
