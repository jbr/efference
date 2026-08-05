//! Tests for `topics.rs`.

use super::*;
use std::time::Duration;

/// Collect state updates until the stream goes quiet, keyed by topic (last one wins —
/// the same coalescing a connection does, and legal for the same reason).
async fn drain(rx: &mut broadcast::Receiver<StateUpdate>) -> HashMap<Topic, StateUpdate> {
    let mut seen = HashMap::new();
    while let Ok(Ok(update)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        seen.insert(update.topic, update);
    }
    seen
}

/// The task publishes every topic once at startup, then **only** what changed.
///
/// The second half is the property that makes level-triggered emission viable at all: if
/// a quiet daemon re-published topics, `?include=state` would be a firehose on a watch
/// rather than the reason that subscription exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishes_the_full_set_once_then_only_changes() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut rx = shared.subscribe_state();
    tokio::spawn(watch_topics(Arc::clone(&shared)));

    let first = drain(&mut rx).await;
    assert_eq!(
        first.len(),
        Topic::ALL.len(),
        "every topic is published once, so a connecting client can be sent a full snapshot"
    );

    // Nothing moved: a ping must not republish. This is the anti-firehose assertion —
    // `touch_state` is deliberately over-eager, and the diff is what absorbs that.
    shared.touch_state();
    assert!(drain(&mut rx).await.is_empty(), "unchanged → silent");

    // One dial moves; only its topic is republished.
    shared.set_review_level(crate::policy::ReviewLevel::HumanOnly);
    let changed = drain(&mut rx).await;
    assert_eq!(changed.len(), 1, "only the topic that moved: {changed:?}");
    let update = &changed[&Topic::ReviewLevel];
    assert_eq!(&*update.json, r#""human_only""#);
}

/// A published value is the topic's **whole** current value — the same bytes
/// `GET /state` carries under that key. This is the fold-equals-snapshot invariant at
/// the source: both sides come from `topic_json`, so they cannot drift.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_published_value_equals_that_key_of_get_state() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut rx = shared.subscribe_state();
    tokio::spawn(watch_topics(Arc::clone(&shared)));

    shared.set_pace(Some(Duration::from_millis(2500)), Some(true));
    let published = drain(&mut rx).await;

    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    for (topic, update) in &published {
        let from_frame: Value = serde_json::from_str(&update.json).unwrap();
        assert_eq!(
            &from_frame,
            &state[topic.as_str()],
            "topic {} differs between the frame and GET /state",
            topic.as_str()
        );
    }
}

/// Every update carries the log position its value reflects, and that position is never
/// *older* than the value — the ordering that makes the watermark safe to gate on.
///
/// A stamp that ran ahead of the values would only delay a frame; one that lagged them
/// would let a client render state accounting for an entry it has not been sent, which is
/// the failure the read-values-then-`last_seq` order exists to prevent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updates_are_stamped_with_the_log_position_they_reflect() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut rx = shared.subscribe_state();
    tokio::spawn(watch_topics(Arc::clone(&shared)));

    // An empty log has no position: nothing for a connection to wait behind.
    let first = drain(&mut rx).await;
    assert!(
        first.values().all(|u| u.watermark.is_none()),
        "empty log → no watermark"
    );

    // Append, then move a topic. The update reflects a log that has three entries, so it
    // must not claim to predate them.
    for i in 0..3 {
        shared
            .append(
                1,
                Payload::User(vec![crate::log::ContentBlock::Text(format!("e{i}"))]),
            )
            .unwrap();
    }
    shared.set_review_level(crate::policy::ReviewLevel::Autonomous);

    let changed = drain(&mut rx).await;
    let update = changed
        .get(&Topic::ReviewLevel)
        .expect("the dial that moved");
    assert_eq!(
        update.watermark,
        Some(2),
        "stamped with the log head at the time the values were read"
    );
}

/// An append alone wakes the diff, without any mutation site pinging `state_changed`.
///
/// This is what covers the two topics whose mutating code holds an `Arc` but no
/// [`Shared`] — the upstream-failure cell and the cost cell — and it is why a missed ping
/// is "late" rather than "stale forever".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_append_alone_wakes_the_diff() {
    // The upstream-failure cell is reached through its own `Arc`, exactly as the HTTP
    // client reaches it — no `Shared` in hand, so no way to ping.
    let failure = crate::api::upstream_failure_state();
    let (shared, _turns_rx) = Shared::with_role_scratch_and_upstream_failure(
        Log::in_memory(),
        LoadedConfig::default(),
        None,
        Arc::clone(&failure),
    );
    let mut rx = shared.subscribe_state();
    tokio::spawn(watch_topics(Arc::clone(&shared)));
    drain(&mut rx).await; // the startup snapshot

    *failure.lock().unwrap() = Some(crate::api::UpstreamFailure {
        status: 429,
        count: 2,
    });
    // Then append, as a request that failed and retried would.
    shared
        .append(
            1,
            Payload::User(vec![crate::log::ContentBlock::Text("x".into())]),
        )
        .unwrap();

    let changed = drain(&mut rx).await;
    assert!(
        changed.contains_key(&Topic::UpstreamFailure),
        "the append repaired a topic that never pinged: {changed:?}"
    );
}
