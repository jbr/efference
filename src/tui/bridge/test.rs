use std::collections::HashSet;

use serde_json::json;

use super::*;
use crate::protocol::state::LoopPhase;

/// What `data` (one frame's JSON) sends, and the process ids it asks to seed.
fn decode(data: &serde_json::Value, seeded: &mut HashSet<u64>) -> (Vec<TuiEvent>, Vec<u64>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let Continue(fresh) = decode_frame(&data.to_string(), seeded, &tx) else {
        panic!("the receiver is alive");
    };
    (rx.try_iter().collect(), fresh)
}

fn state(kind: &str, data: &serde_json::Value) -> serde_json::Value {
    json!({"class": "state", "kind": kind, "data": data})
}

/// A `state` frame is one event carrying the topic's typed whole value, and a `null` frame
/// carries the clear.
#[test]
fn a_state_frame_is_one_topic_event() {
    let mut seeded = HashSet::new();
    let (events, fresh) = decode(
        &state("loop_phase", &json!({"phase": "delayed", "until": 5})),
        &mut seeded,
    );
    assert!(fresh.is_empty());
    assert!(matches!(
        events.as_slice(),
        [TuiEvent::Topic(TopicUpdate::LoopPhase(
            LoopPhase::Delayed { until: 5 }
        ))]
    ));

    let (events, _) = decode(&state("gate", &json!(null)), &mut seeded);
    assert!(matches!(
        events.as_slice(),
        [TuiEvent::Topic(TopicUpdate::Gate(None))]
    ));
}

/// A frame the TUI cannot use is skipped, not fatal: the stream goes on.
#[test]
fn what_the_tui_cannot_read_is_skipped() {
    let mut seeded = HashSet::new();
    for frame in [
        state("a_topic_from_the_future", &json!({"anything": true})),
        state("queued", &json!([])),
        state("pace", &json!({"speed_ms": "fast"})),
        json!({"class": "a_class_from_the_future", "kind": "x", "data": {}}),
    ] {
        let (events, fresh) = decode(&frame, &mut seeded);
        assert!(events.is_empty() && fresh.is_empty(), "{frame}");
    }
    let (tx, _rx) = crossbeam_channel::unbounded();
    assert!(matches!(
        decode_frame("not json", &mut seeded, &tx),
        Continue(fresh) if fresh.is_empty()
    ));
}

/// A listed process is seeded from the process resource once, when it first becomes
/// visible: a foreground process is not listed, and one that flips to background is seeded
/// then, not again on the frames that follow.
#[test]
fn a_process_is_seeded_when_it_first_becomes_listed() {
    let process = |id: u64, background: bool, muted: bool| {
        json!({
            "id": id, "command": "sleep 60", "call_id": null, "pid": 7,
            "spawned_at": 1, "background": background, "muted": muted,
        })
    };
    let mut seeded = HashSet::new();

    let (events, fresh) = decode(
        &state(
            "processes",
            &json!([process(1, false, false), process(2, true, true)]),
        ),
        &mut seeded,
    );
    assert_eq!(events.len(), 1);
    assert!(fresh.is_empty(), "foreground and muted rows are not listed");

    let (_, fresh) = decode(
        &state(
            "processes",
            &json!([process(1, true, false), process(2, true, true)]),
        ),
        &mut seeded,
    );
    assert_eq!(fresh, [1], "backgrounding lists it");

    let (_, fresh) = decode(
        &state(
            "processes",
            &json!([process(1, true, false), process(3, true, false)]),
        ),
        &mut seeded,
    );
    assert_eq!(fresh, [3], "a listed row is seeded once");
}

/// The decode sends the events the UI folds: a log entry and an output delta still arrive
/// as theirs.
#[test]
fn log_and_output_frames_keep_their_events() {
    let mut seeded = HashSet::new();
    let (events, _) = decode(
        &json!({"class": "output", "kind": "assistant_delta", "data": {"index": 0, "text": "hi"}}),
        &mut seeded,
    );
    assert!(matches!(
        events.as_slice(),
        [TuiEvent::AssistantDelta(text)] if text == "hi"
    ));
}
