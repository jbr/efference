//! Tests for `notifications.rs`.

use super::*;

/// A notification with a slot waits in memory and supersedes by slot (archive-and-replace,
/// never edit — docs/inbox.md, docs/log-and-projection.md); a notification without one is
/// a log entry from its arrival and answers with that entry's seq.
#[test]
fn slot_notifications_queue_and_supersede_and_the_rest_are_logged_at_arrival() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let a = shared.notify(
        "test-watch".into(),
        "running 5min".into(),
        Some("bash#7".into()),
    );
    assert_eq!(a, None, "a slot notification has no entry yet");
    assert!(shared.snapshot().is_empty(), "nothing logged for a slot");
    let b = shared
        .notify("reviewer".into(), "nit at L40".into(), None)
        .expect("an arrival answers with its seq");
    let logged = shared.snapshot();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].seq, b);
    assert!(matches!(&logged[0].payload, Payload::Ambient(n) if n.body == "nit at L40"));
    assert_eq!(shared.pending_notifications().len(), 2);

    // Same slot → replace, not append (the "running 5min" → "finished" case).
    shared.notify(
        "test-watch".into(),
        "finished".into(),
        Some("bash#7".into()),
    );
    let pending = shared.pending_notifications();
    assert_eq!(pending.len(), 2, "slot supersession replaced, didn't add");
    assert!(pending.iter().any(|n| n.body == "finished"));
    assert!(
        !pending.iter().any(|n| n.body == "running 5min"),
        "superseded entry gone"
    );
    assert_eq!(
        pending.iter().find(|n| n.body == "nit at L40").unwrap().seq,
        Some(b),
        "an arrived notification is addressed by its seq"
    );
}

/// A must-wake notification (docs/inbox.md) carries its flag on its arrival entry, so a
/// restart keeps it, and through the queue for a slot notification: the `GET /state`
/// view (present only when set), slot supersession, and the drain.
#[test]
fn a_wake_notification_keeps_its_flag() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.notify("op".into(), "fyi".into(), None);
    shared.notify_wake("child".into(), "stuck".into(), None);
    shared.notify_wake("child".into(), "gated".into(), Some("session:child".into()));
    let json = serde_json::to_value(shared.pending_notifications()).unwrap();
    assert_eq!(json[0].get("wake"), None, "absent unless set: {json}");
    assert_eq!(json[1]["wake"], true, "{json}");
    assert_eq!(json[2]["wake"], true, "{json}");

    let logged: Vec<bool> = shared
        .snapshot()
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Ambient(n) => Some(n.wake),
            _ => None,
        })
        .collect();
    assert_eq!(logged, [false, true], "the flag is on the arrival entry");

    // Supersession on the slot keeps the replacement's own terms.
    shared.notify_wake("child".into(), "idle".into(), Some("session:child".into()));
    let drained = shared.drain_inbox();
    assert_eq!(drained.len(), 1);
    assert!(drained[0].wake);
    assert_eq!(drained[0].body, "idle");
}

/// `inbox_wake` (docs/inbox.md — the idle wake signal) resolves immediately when a
/// notification has not been shown, and otherwise unblocks when one arrives. A triage's
/// mark (`shown_through`) covering it ends the wake, so a deferred item can't spin the
/// loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_wake_fires_on_unshown_not_on_shown() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    // Already arrived → resolves at once.
    let seq = shared
        .notify("ci".into(), "failed".into(), None)
        .expect("logged at arrival");
    tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
        .await
        .expect("woke on an unshown notification");

    // A triage that showed it (it records the highest arrival it held) → the wake blocks…
    shared
        .append(
            0,
            Payload::Triage(crate::log::Triage {
                for_result: None,
                origin: crate::log::Origin::Model,
                raw_keep: String::new(),
                inbox: Vec::new(),
                surfaced: Vec::new(),
                retries: Vec::new(),
                thinking: Vec::new(),
                shown_through: Some(seq),
            }),
        )
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), shared.inbox_wake())
            .await
            .is_err(),
        "a shown notification must not keep waking the idle loop"
    );

    // …until a fresh one arrives.
    let s2 = Arc::clone(&shared);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s2.notify("op".into(), "nudge".into(), None);
    });
    tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
        .await
        .expect("woke on a freshly-arrived notification");
}

/// `POST /notifications` answers with the arrival's seq (nothing for a slot), slot
/// supersession and `GET /state` exposure run over the real router + control client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_and_supersede_over_the_router() {
    let log_path = std::env::temp_dir().join("efference-notify-test.jsonl");
    let _ = std::fs::remove_file(&log_path);

    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
    let client = test_client(&shared).await;
    let first = client
        .notify("test-watch", "running 5min", Some("bash#7"))
        .await
        .expect("notify");
    assert_eq!(first, None, "a slot notification has no seq yet");
    // Same slot supersedes the first.
    client
        .notify("test-watch", "finished, 0 failures", Some("bash#7"))
        .await
        .expect("notify 2");
    let seq = client
        .notify("reviewer", "nit at L40", None)
        .await
        .expect("notify 3")
        .expect("an arrival answers with its seq");

    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    let notes = state["notifications"].as_array().unwrap();
    assert_eq!(notes.len(), 2, "slot superseded, not appended: {notes:?}");
    assert_eq!(notes[0]["seq"], seq);
    assert_eq!(notes[0]["source"], "reviewer");
    assert_eq!(notes[1]["body"], "finished, 0 failures");
    assert_eq!(notes[1].get("seq"), None, "{notes:?}");

    let _ = std::fs::remove_file(&log_path);
}

/// What a restart keeps (docs/inbox.md, docs/resumption.md): notifications without a slot
/// are in the log from their arrival and come back pending, must-wake flag included; a
/// slot notification was only ever in memory and does not.
#[test]
fn arrived_notifications_survive_a_reload_and_slot_ones_do_not() {
    let path = std::env::temp_dir().join("efference-durable-inbox-test.jsonl");
    let _ = std::fs::remove_file(&path);
    let kept = {
        let (shared, _turns_rx) = Session::new(
            std::env::current_dir().unwrap(),
            Log::create(&path, None).expect("create log"),
        );
        let kept = shared
            .notify("agent:reviewer".into(), "please look".into(), None)
            .unwrap();
        shared.notify_wake("child".into(), "stuck".into(), None);
        shared.notify("ci".into(), "running 19s".into(), Some("p#5".into()));
        shared.close_log();
        kept
    };

    let opened = Log::open(&path).expect("reopen");
    let (reloaded, _turns_rx) = Session::new(std::env::current_dir().unwrap(), opened.log);
    let pending = reloaded.pending_notifications();
    assert_eq!(pending.len(), 2, "{pending:?}");
    assert_eq!(pending[0].seq, Some(kept));
    assert_eq!(pending[0].body, "please look");
    assert!(!pending[0].wake);
    assert!(pending[1].wake, "must-wake survives the restart");
    assert!(reloaded.has_pending_notifications());
    reloaded.close_log();
    let _ = std::fs::remove_file(&path);
}
