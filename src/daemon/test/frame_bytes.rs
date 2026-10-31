//! The bytes each `output` frame serializes to (docs/control-api.md), one pin per kind,
//! and the `kind` every log frame carries. The clients parse these, so a change to them
//! is a change to the control channel and fails here first, as the state topics' bytes
//! do (`topic_bytes.rs`).
//!
//! The comparison is of whole strings, key order included: the frame's own keys
//! (`class`, `kind`, `data`) in the order the envelope declares them, and the keys of
//! `data` sorted, as every object the channel builds from a `serde_json::Value` is.
//! Each input carries a character JSON escapes, so the escaping is pinned with the
//! keys.

use super::*;
use crate::log::{
    AssistantOutput, ColdEvent, ConfigChange, ConfigEdit, ContinueOrigin, Escalation, ExitOutcome,
    GateDecision, InterruptOrigin, MomentKind, Notification, ProcessEvent, ProcessEventKind,
    ProcessExit, PushEvent, PushRecord, ReleasedBy, RequestStage, RequestStageKind, Resumed, Spot,
    Stance, Summary, Telemetry, Timestamp, Title, TitleAuthor, ToolOutcome, ToolTelemetry, Triage,
    WireDialect, WireState,
};
use crate::protocol::stream::StreamDelta;

fn frame(delta: &StreamDelta) -> String {
    Frame::output(delta).to_json()
}

#[test]
fn the_model_stream_frames_keep_their_bytes() {
    assert_eq!(
        frame(&StreamDelta::AssistantText {
            index: 0,
            text: "say \"hi\"\n".into(),
        }),
        r#"{"class":"output","kind":"assistant_delta","data":{"index":0,"text":"say \"hi\"\n"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ThinkingText {
            index: 2,
            text: "hmm — é".into(),
        }),
        r#"{"class":"output","kind":"thinking_delta","data":{"index":2,"text":"hmm — é"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ToolCallStart {
            index: 1,
            call_id: "toolu_1".into(),
            name: "bash".into(),
        }),
        r#"{"class":"output","kind":"tool_call_start","data":{"call_id":"toolu_1","index":1,"name":"bash"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ToolCallInput {
            index: 1,
            fragment: "{\"command\":\"ls".into(),
        }),
        r#"{"class":"output","kind":"tool_call_input","data":{"fragment":"{\"command\":\"ls","index":1}}"#
    );
}

#[test]
fn the_tool_and_process_frames_keep_their_bytes() {
    assert_eq!(
        frame(&StreamDelta::ToolOutput {
            call_id: "toolu_1".into(),
            chunk: "a\tb\n".into(),
        }),
        r#"{"class":"output","kind":"tool_output","data":{"call_id":"toolu_1","chunk":"a\tb\n"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ProcessOutput {
            id: 3,
            chunk: "ready\n".into(),
        }),
        r#"{"class":"output","kind":"process_output","data":{"chunk":"ready\n","id":3}}"#
    );
}

#[test]
fn the_side_request_frames_keep_their_bytes() {
    assert_eq!(
        frame(&StreamDelta::CompactionText {
            text: "the summary".into(),
        }),
        r#"{"class":"output","kind":"compaction_delta","data":{"text":"the summary"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::CompactionThinking {
            text: "weighing".into(),
        }),
        r#"{"class":"output","kind":"compaction_thinking_delta","data":{"text":"weighing"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ConsolidationText {
            text: "the account".into(),
        }),
        r#"{"class":"output","kind":"consolidation_delta","data":{"text":"the account"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ConsolidationThinking {
            text: "sorting".into(),
        }),
        r#"{"class":"output","kind":"consolidation_thinking_delta","data":{"text":"sorting"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::TriageText {
            call_ids: vec!["toolu_1".into(), "toolu_2".into()],
            text: "keep 1-3".into(),
        }),
        r#"{"class":"output","kind":"triage_delta","data":{"call_ids":["toolu_1","toolu_2"],"text":"keep 1-3"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::TriageThinking {
            call_ids: Vec::new(),
            text: "idle".into(),
        }),
        r#"{"class":"output","kind":"triage_thinking_delta","data":{"call_ids":[],"text":"idle"}}"#
    );
}

/// One payload of each `Payload` and `Telemetry` variant, with the kind it frames under
/// (docs/log-and-projection.md). Shared by the two tests below: the bytes one pins what
/// each kind serializes to, and the topic one checks the set against the state topics.
fn log_kinds() -> Vec<(Payload, &'static str)> {
    let telemetry = |t: Telemetry| Payload::Telemetry(t);
    vec![
        (Payload::user("hi"), "user"),
        (
            Payload::assistant(AssistantOutput::empty(WireDialect::Messages)),
            "assistant",
        ),
        (
            Payload::tool_result("toolu_1", RawResult::text("ok"), WireState::Raw),
            "tool_result",
        ),
        (Payload::result_verdict(2, WireState::Raw), "result_verdict"),
        (
            Payload::Summary(Summary::consolidation(0, 3, "the account")),
            "summary",
        ),
        (Payload::Triage(Triage::new(Some(2), "all")), "triage"),
        (
            Payload::Ambient(Notification::new("web", "a note", None)),
            "ambient",
        ),
        (
            Payload::Spot(Spot::new("toolu_1", Stance::Clear, "in scope")),
            "spot",
        ),
        (Payload::Resumed(Resumed::new(1_000)), "resumed"),
        (Payload::archived(None), "archived"),
        (
            Payload::ProcessEvent(ProcessEvent {
                id: 1,
                kind: ProcessEventKind::Exited(ProcessExit {
                    outcome: ExitOutcome::Code(0),
                    duration_ms: 10,
                    at: Timestamp(0),
                }),
            }),
            "process_event",
        ),
        (
            Payload::Worktree(
                crate::worktree::Retired::Removed {
                    branch_deleted: true,
                }
                .into_event(std::path::Path::new("/p"), "quiet-fox")
                .expect("a removal is recorded"),
            ),
            "worktree",
        ),
        (
            Payload::ImagesRefused {
                refusal: "no images".into(),
            },
            "images_refused",
        ),
        (
            Payload::ConfigEdit(ConfigEdit::new(
                EditState::Committed,
                ConfigChange::Command {
                    command: "ls".into(),
                },
            )),
            "config_edit",
        ),
        (
            Payload::preamble("You are m.", " …", Default::default(), None),
            "preamble",
        ),
        (
            Payload::Title(Title::new("a title", TitleAuthor::Operator)),
            "title",
        ),
        (
            telemetry(Telemetry::Tool(ToolTelemetry {
                call_id: "toolu_1".into(),
                tool: "read".into(),
                duration_ms: 1,
                outcome: ToolOutcome::Applied,
                result_bytes: 2,
                images: 0,
                bash: None,
                edit: None,
            })),
            "telemetry.tool",
        ),
        (
            telemetry(Telemetry::call_started("toolu_1")),
            "telemetry.call_started",
        ),
        (
            telemetry(Telemetry::RequestStage(RequestStage::new(
                0,
                RequestOrigin::Turn,
                RequestStageKind::Sent,
            ))),
            "telemetry.request_stage",
        ),
        (
            telemetry(Telemetry::Push(PushRecord {
                moment: 1,
                kind: MomentKind::Gate,
                event: PushEvent::Sent,
            })),
            "telemetry.push",
        ),
        (
            telemetry(Telemetry::Cold(ColdEvent::Released {
                by: ReleasedBy::Turn,
            })),
            "telemetry.cold",
        ),
        (
            telemetry(Telemetry::CompactionRequested { warm: true, cut: 3 }),
            "telemetry.compaction_requested",
        ),
        (
            telemetry(Telemetry::CompactionFailed {
                error: "superseded".into(),
            }),
            "telemetry.compaction_failed",
        ),
        (
            telemetry(Telemetry::InboxExpired { seq: 4, shows: 2 }),
            "telemetry.inbox_expired",
        ),
        (
            telemetry(Telemetry::SupervisorSet { supervisor: None }),
            "telemetry.supervisor_set",
        ),
        (
            telemetry(Telemetry::Interrupt(InterruptOrigin::default())),
            "telemetry.interrupt",
        ),
        (
            telemetry(Telemetry::Continue(ContinueOrigin::default())),
            "telemetry.continue",
        ),
        (
            telemetry(Telemetry::GateDecision(GateDecision::new(
                "toolu_1",
                Approval::Approve,
                None,
            ))),
            "telemetry.gate_decision",
        ),
        (
            telemetry(Telemetry::Escalation(Escalation::new(
                "toolu_1", None, None, None,
            ))),
            "telemetry.escalation",
        ),
        (
            telemetry(Telemetry::Consolidation {
                from: 1,
                upto: 3,
                context: 4,
                tool_bytes: 100,
                outcome: "checking".into(),
                account_bytes: 0,
            }),
            "telemetry.consolidation",
        ),
        (
            telemetry(Telemetry::Tools { tools: Vec::new() }),
            "telemetry.tools",
        ),
    ]
}

/// The `kind` of the log frame each payload rides in, which is also its SSE `event:`
/// (docs/control-api.md): the payload variant's name, and for the telemetry family the
/// telemetry variant's under a `telemetry.` prefix (docs/log-and-projection.md). Pinned
/// as the frame carries it, ahead of `seq` and `data`.
#[test]
fn the_log_frame_kinds_keep_their_bytes() {
    for (seq, (payload, kind)) in (0u64..).zip(log_kinds()) {
        let frame = Frame::log(&LogEntry::new(seq, payload), None, None);
        let json = frame.to_json();
        let head = format!(r#"{{"class":"log","kind":"{kind}","seq":{seq},"data":"#);
        assert!(json.starts_with(&head), "{kind}: {json}");
        assert_eq!(
            stream::FrameEvent::from(frame).event_type(),
            Some(kind),
            "the SSE event names the frame's kind"
        );
    }
}

/// A log frame's `kind` is also its SSE `event:`, and a state frame's is the topic's
/// name, so a kind held by both would let a client take a log entry for a state update
/// (docs/control-api.md). The two sets stay disjoint: a log payload variant named after
/// a topic fails here before the channel carries the collision.
#[test]
fn no_log_frame_kind_is_named_like_a_state_topic() {
    for (payload, _) in log_kinds() {
        let kind = frames::payload_kind(&payload);
        // Against the names `Topic::ALL` carries rather than `Topic::parse`, so the
        // check cannot pass by the lookup answering `None` for every name.
        assert!(
            !Topic::ALL.iter().any(|topic| topic.as_str() == kind),
            "{kind} names both a log frame's kind and a state topic"
        );
    }
}
