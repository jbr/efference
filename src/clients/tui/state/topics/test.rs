use serde_json::json;

use crate::clients::tui::harness::{Screenplay, gate, process, session_identity};
use crate::clients::tui::state::{Exit, IDLE_STATUS};
use crate::clients::tui::topics::TopicUpdate;
use crate::inference::api::UpstreamFailure;
use crate::protocol::state::ProcessState;
use crate::protocol::state::{LoopPhase, PaceView, SessionIdentity, UsageView};
use crate::record::log::Timestamp;

/// Every topic frame asks for a repaint, and none but the gate and the ask asks for a
/// reprojection of the transcript.
#[test]
fn a_frame_asks_for_a_repaint() {
    let frames = [
        TopicUpdate::Usage(Some(UsageView::default())),
        TopicUpdate::Pace(Some(PaceView {
            speed_ms: 0,
            held: false,
        })),
        TopicUpdate::UpstreamFailure(Some(UpstreamFailure::new(Some(529), "overloaded"))),
        TopicUpdate::Cold(None),
        TopicUpdate::LoopPhase(LoopPhase::Requesting),
        TopicUpdate::Processes(vec![process(1, "sleep 60")]),
        TopicUpdate::SessionIdentity(Some(session_identity("amber-heron", "m"))),
    ];
    let mut play = Screenplay::new(80, 12);
    for frame in frames {
        play.state.dirty = false;
        play.state.reproject_pending = false;
        let name = format!("{frame:?}");
        play.topic(frame);
        assert!(play.state.dirty, "{name} asks for a repaint");
        assert!(
            !play.state.reproject_pending,
            "{name} leaves the transcript"
        );
    }

    play.state.reproject_pending = false;
    play.topic(TopicUpdate::Gate(Some(gate("toolu_1", "bash", json!({})))));
    assert!(
        play.state.reproject_pending,
        "a gate changes the call it holds"
    );
}

/// The gate says so on the status line while it is held and takes the hint back when it
/// resolves.
#[test]
fn the_gate_is_announced_and_withdrawn() {
    let mut play = Screenplay::new(80, 12);
    play.topic(TopicUpdate::Gate(Some(gate(
        "toolu_1",
        "bash",
        json!({"command": "rm -rf ./build"}),
    ))));
    assert_eq!(
        &*play.state.ui.debug_message,
        "approval needed: bash — ⏎ approve · type to redirect"
    );
    play.topic(TopicUpdate::Gate(None));
    assert_eq!(&*play.state.ui.debug_message, IDLE_STATUS);
    assert!(play.state.topics.gate.is_none());
}

/// The panel lists the background processes the operator has not dismissed. A frame
/// replaces the list; a process that is still there keeps the tail this client collected
/// for it, and one that left takes its tail along.
#[test]
fn the_panel_keeps_the_tail_of_a_process_across_frames() {
    let mut play = Screenplay::new(80, 12);
    let foreground = ProcessState {
        background: false,
        ..process(1, "cargo test")
    };
    let muted = ProcessState {
        muted: true,
        ..process(2, "tail -f log")
    };
    play.processes(vec![
        foreground.clone(),
        muted.clone(),
        process(3, "cargo watch"),
        process(4, "npm run dev"),
    ]);
    let ids: Vec<u64> = play
        .state
        .process_panel
        .iter()
        .map(|p| p.process.id)
        .collect();
    assert_eq!(
        ids,
        [3, 4],
        "the foreground tool and a muted process are not listed"
    );
    assert_eq!(
        play.state.topics.processes.len(),
        4,
        "the store holds the topic whole"
    );

    play.process_output(3, "Compiling\n");
    play.process_output(4, "ready\n");
    play.process_output(1, "not in the panel\n");

    // Process 4 exits, 2 is un-muted, 3 carries on.
    play.processes(vec![
        ProcessState {
            muted: false,
            ..muted
        },
        process(3, "cargo watch"),
    ]);
    let rows: Vec<(u64, &str)> = play
        .state
        .process_panel
        .iter()
        .map(|p| (p.process.id, p.tail.as_str()))
        .collect();
    assert_eq!(rows, [(2, ""), (3, "Compiling\n")]);
    assert!(play.state.process_panel[1].last_chunk_at.is_some());

    // A process that comes back is a new row.
    play.processes(vec![process(4, "npm run dev")]);
    assert_eq!(play.state.process_panel[0].tail, "");
}

/// The time since the last request derives from the instant the `usage` topic carries,
/// and again on the clock tick; an unchanged tick reports nothing to repaint.
#[test]
fn the_time_since_the_last_request_follows_the_usage_topic_and_the_clock() {
    let mut play = Screenplay::new(80, 12);
    assert_eq!(play.state.since_last_request, None);

    let seven_seconds_ago = Timestamp(Timestamp::now().0 - 7_000);
    play.topic(TopicUpdate::Usage(Some(UsageView {
        last_request_at: Some(seven_seconds_ago),
        ..UsageView::default()
    })));
    assert_eq!(play.state.since_last_request, Some(7));
    assert!(
        !play.state.refresh_clocks(),
        "nothing moved within the second"
    );

    play.state.since_last_request = Some(3);
    assert!(
        play.state.refresh_clocks(),
        "the tick corrects a stale value"
    );
    assert_eq!(play.state.since_last_request, Some(7));

    play.topic(TopicUpdate::Usage(Some(UsageView::default())));
    assert_eq!(
        play.state.since_last_request, None,
        "before the first request"
    );
}

/// The pace keys write the dial ahead of the topic that confirms it, and the next frame
/// replaces what they wrote.
#[test]
fn a_pace_frame_replaces_the_value_a_key_wrote() {
    let mut play = Screenplay::new(80, 12);
    play.topic(TopicUpdate::Pace(Some(PaceView {
        speed_ms: 2_000,
        held: false,
    })));
    play.state.toggle_pace_hold();
    assert_eq!(
        play.state.topics.pace,
        Some(PaceView {
            speed_ms: 2_000,
            held: true
        }),
        "the key holds the loop at once"
    );
    assert_eq!(play.took_pace(), Some((None, Some(true))));

    play.topic(TopicUpdate::Pace(Some(PaceView {
        speed_ms: 5_000,
        held: false,
    })));
    assert_eq!(
        play.state.topics.pace,
        Some(PaceView {
            speed_ms: 5_000,
            held: false
        }),
        "the host's value is the one that stands"
    );
}

/// A session that names a successor is followed once: the destination is the first one
/// set, and the loop is asked to leave.
#[test]
fn a_successor_in_the_identity_ends_the_loop_once() {
    let mut play = Screenplay::new(80, 12);
    let handing_off = |successor: &str| SessionIdentity {
        successor: Some(successor.into()),
        ..session_identity("old-leaf", "m")
    };
    assert!(!play.event_quits(crate::clients::tui::TuiEvent::Topic(
        TopicUpdate::SessionIdentity(Some(session_identity("old-leaf", "m")))
    )));
    assert!(play.event_quits(crate::clients::tui::TuiEvent::Topic(
        TopicUpdate::SessionIdentity(Some(handing_off("new-leaf")))
    )));
    assert_eq!(
        play.state.switch_to,
        Some(Exit::Switch {
            id: "new-leaf".into()
        })
    );
    // A destination already chosen is not replaced by a later identity.
    assert!(!play.event_quits(crate::clients::tui::TuiEvent::Topic(
        TopicUpdate::SessionIdentity(Some(handing_off("newer-leaf")))
    )));
    assert_eq!(
        play.state.switch_to,
        Some(Exit::Switch {
            id: "new-leaf".into()
        })
    );
}
