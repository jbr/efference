//! Tests for `notifications.rs`.

use super::*;

/// The inbox supersedes by slot (archive-and-replace, never edit — docs/inbox.md, docs/log-and-projection.md) and
/// dismisses by id; a superseded or dismissed entry is gone from the pending set.
#[test]
fn inbox_supersedes_by_slot_and_dismisses() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let _a = shared.notify(
        "test-watch".into(),
        "running 5min".into(),
        Some("bash#7".into()),
        None,
    );
    let b = shared.notify("reviewer".into(), "nit at L40".into(), None, None);
    assert_eq!(shared.pending_notifications().len(), 2);

    // Same slot → replace, not append (the "running 5min" → "finished" case).
    let _c = shared.notify(
        "test-watch".into(),
        "finished".into(),
        Some("bash#7".into()),
        None,
    );
    let pending = shared.pending_notifications();
    assert_eq!(pending.len(), 2, "slot supersession replaced, didn't add");
    assert!(pending.iter().any(|n| n.body == "finished"));
    assert!(
        !pending.iter().any(|n| n.body == "running 5min"),
        "superseded entry gone"
    );

    // Explicit dismiss by id; a second dismiss of the same id is a no-op.
    assert!(shared.dismiss_notification(b));
    assert!(!shared.dismiss_notification(b));
    let pending = shared.pending_notifications();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].body, "finished");
}

/// `inbox_wake` (docs/inbox.md — the idle wake signal) resolves immediately when a
/// notification is already pending, and otherwise unblocks when one arrives. Drained
/// (shown) notifications don't re-trigger it, so a deferred item can't spin the loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_wake_fires_on_pending_not_on_drained() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());

    // Already pending → resolves at once.
    shared.notify("ci".into(), "failed".into(), None, None);
    tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
        .await
        .expect("woke on an existing pending notification");

    // Drain it (now "shown") → the queue is empty, so the wake blocks…
    let _ = shared.drain_inbox();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), shared.inbox_wake())
            .await
            .is_err(),
        "a drained notification must not keep waking the idle loop"
    );

    // …until a fresh one arrives.
    let s2 = Arc::clone(&shared);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s2.notify("op".into(), "nudge".into(), None, None);
    });
    tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
        .await
        .expect("woke on a freshly-arrived notification");
}

/// `POST /notifications`, slot supersession, `GET /state` exposure, and
/// `DELETE /notifications/:id` over the real router + control client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_supersede_and_dismiss_over_the_router() {
    let log_path = std::env::temp_dir().join("efference-notify-test.jsonl");
    let _ = std::fs::remove_file(&log_path);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    let client = test_client(&shared).await;
    let gone = client
        .notify("test-watch", "running 5min", Some("bash#7"), None)
        .await
        .expect("notify");
    // Same slot supersedes the first.
    client
        .notify("test-watch", "finished, 0 failures", Some("bash#7"), None)
        .await
        .expect("notify 2");

    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    let notes = state["notifications"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "slot superseded, not appended: {notes:?}");
    assert_eq!(notes[0]["body"], "finished, 0 failures");
    assert_eq!(notes[0]["source"], "test-watch");
    let live_id = notes[0]["id"].as_u64().unwrap();

    // Dismissing the live one empties the inbox; dismissing the superseded id 404s.
    client.dismiss_notification(live_id).await.unwrap();
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert!(state["notifications"].as_array().unwrap().is_empty());
    assert!(
        client.dismiss_notification(gone).await.is_err(),
        "the superseded id is gone → 404"
    );

    let _ = std::fs::remove_file(&log_path);
}
