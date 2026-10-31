//! The rule `wait` reports by: a status is new when it is a decision point the mount has
//! not shown of that session (docs/operating.md). Views are read from `state` frames, as
//! the follower reads them.

use super::*;
use serde_json::json;

/// A `status` frame for `child` with `condition`, as the stream carries it.
fn frame(condition: &serde_json::Value) -> String {
    json!({
        "class": "state",
        "kind": "status",
        "data": { "id": "child", "condition": condition, "footer": {} },
    })
    .to_string()
}

fn view(condition: &serde_json::Value) -> StatusView {
    status_frame(&frame(condition)).expect("a status frame")
}

fn gated(call_id: &str, reviewer: &serde_json::Value) -> StatusView {
    view(&json!({
        "kind": "gated",
        "call_id": call_id,
        "tool": "bash",
        "handle": "3",
        "reviewer": reviewer,
    }))
}

fn idle(seq: u64) -> StatusView {
    view(&json!({
        "kind": "idle",
        "closing": { "seq": seq, "text": "done", "bytes": 4, "lines": 1 },
    }))
}

fn working() -> StatusView {
    view(&json!({ "kind": "working", "activity": [{ "kind": "thinking", "since": 1 }] }))
}

/// Only a `status` state frame carries a view.
#[test]
fn only_status_frames_are_read() {
    let gate = json!({ "class": "state", "kind": "gate", "data": null }).to_string();
    assert!(status_frame(&gate).is_none());
    let log = json!({ "class": "log", "kind": "user", "seq": 0, "data": {} }).to_string();
    assert!(status_frame(&log).is_none());
}

/// A working or starting session is never news; a gate is told apart by its call and
/// its reviewer, an idle by its closing message, and every end is one end.
#[test]
fn marks_tell_decision_points_apart() {
    let operator = || json!({ "kind": "operator" });
    assert_eq!(mark(&working()), None);
    let starting = view(&json!({ "kind": "starting", "stage": "environment", "since": 1 }));
    assert_eq!(mark(&starting), None);

    assert_eq!(
        mark(&gated("toolu_1", &operator())),
        mark(&gated("toolu_1", &operator()))
    );
    assert_ne!(
        mark(&gated("toolu_1", &operator())),
        mark(&gated("toolu_2", &operator()))
    );
    assert_ne!(
        mark(&gated("toolu_1", &operator())),
        mark(&gated(
            "toolu_1",
            &json!({ "kind": "supervisor", "id": "parent" })
        ))
    );
    assert_eq!(mark(&idle(4)), mark(&idle(4)));
    assert_ne!(mark(&idle(4)), mark(&idle(9)));
    assert_eq!(
        mark(&view(&json!({ "kind": "failed", "error": "boom" }))),
        Some(Mark::Ended)
    );
}

/// One watched session with `shown` as its last shown status, and no follower.
fn watching(shown: StatusView) -> Arc<Watch> {
    let watch = Arc::new(Watch::default());
    let follower = tokio::spawn(std::future::pending::<()>()).abort_handle();
    watch.agents().insert(
        "child".into(),
        Watched {
            shown: mark(&shown),
            latest: Some(shown),
            follower,
        },
    );
    watch
}

/// What is shown is not new again; a working status does not end a wait; a decision
/// point is returned once; an end is returned once and then the session is gone.
#[tokio::test(start_paused = true)]
async fn each_decision_point_is_returned_once() {
    let watch = watching(idle(4));
    let second = Duration::from_secs(1);
    let unchanged = |next: Next| match next {
        Next::Unchanged(agents) => agents,
        _ => panic!("expected nothing new"),
    };
    let new = |next: Next| match next {
        Next::New(views) => views,
        _ => panic!("expected something new"),
    };

    let agents = unchanged(watch.next(second).await);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].1, Some(idle(4)));

    watch.saw("child", working());
    unchanged(watch.next(second).await);

    let gate = gated("toolu_1", &json!({ "kind": "operator" }));
    watch.saw("child", gate.clone());
    assert_eq!(new(watch.next(second).await), [gate]);
    unchanged(watch.next(second).await);

    watch.saw("child", idle(9));
    assert_eq!(new(watch.next(second).await), [idle(9)]);

    watch.ended("child");
    let ended = new(watch.next(second).await);
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].condition, Condition::Ended);
    assert!(matches!(watch.next(second).await, Next::Nothing));
}
