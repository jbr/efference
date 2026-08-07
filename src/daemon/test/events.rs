//! End-to-end tests of `GET /events` over the real router — the SSE hop that
//! `stream.rs`'s in-process tests do not cross.

use super::*;

/// `GET /events` replays the pre-existing log (id == seq) and then follows new
/// appends live — including an append that lands *after* the client has caught
/// up and is awaiting, which is the lost-wakeup path the tail is built to
/// survive (the `Notified` is armed before the catch-up read).
///
/// End-to-end over the real router, so it also pins that the frame envelope survives the
/// HTTP hop: `data` is a frame whose `data` is the entry (`stream.rs` covers the envelope
/// itself). `?include=log` keeps this about the durable class.
///
/// Multi-thread flavor: an SSE body streams server-write ↔ client-read
/// concurrently, which a single-threaded test runtime can deadlock (the real
/// daemon runs on the multi-thread runtime).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_replays_then_tails_live() {
    let log_path = std::env::temp_dir().join("efference-sse-test.jsonl");
    let _ = std::fs::remove_file(&log_path);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    // Two entries present before any client connects (the replay set).
    shared.append(1, user("first")).unwrap();
    shared.append(2, user("second")).unwrap();

    let client = test_client(&shared).await;
    let mut events = client
        .events(Some(0), Some("log"))
        .await
        .expect("open events");

    // Replay: the two pre-existing entries, in order, id == seq, typed `user`.
    let (id, kind, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("0"));
    assert_eq!(kind.as_deref(), Some("user"));
    let frame: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(frame["class"], "log");
    assert!(frame["data"].to_string().contains("first"));
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("1"));
    assert!(data.contains("second"));

    // Live tail: append once the client is caught up and parked on the wait.
    shared.append(3, user("third")).unwrap();
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("2"));
    assert!(data.contains("third"));

    let _ = std::fs::remove_file(&log_path);
}

/// `?include=output` carries ephemeral deltas published after a client subscribes:
/// assistant prose (`assistant_delta`), tool output (`tool_output`), and process
/// output (`bg_output`), each typed by the frame's `kind`, payloaded under the
/// envelope's `data`, and crucially **id-less** — they're pre-commit and have no
/// `seq`, which is the contract that keeps them off the durable resume cursor. The
/// handler subscribes synchronously while building the response, so a publish after
/// `events()` resolves can't race ahead of the subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_output_class_carries_ephemeral_deltas() {
    let log_path = std::env::temp_dir().join("efference-stream-test.jsonl");
    let _ = std::fs::remove_file(&log_path);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    let client = test_client(&shared).await;
    let mut stream = client
        .events(None, Some("output"))
        .await
        .expect("open events");

    // Published only after the subscription exists (the handler subscribed
    // before returning the response headers `events()` just awaited).
    shared.publish_delta(StreamDelta::AssistantText("hello ".into()));
    shared.publish_delta(StreamDelta::ToolOutput {
        call_id: "toolu_5".into(),
        chunk: "line of output\n".into(),
    });
    shared.publish_delta(StreamDelta::BgOutput {
        handle: "bg#1".into(),
        chunk: "tick 3\n".into(),
    });

    // Prose delta: typed, id-less, text under the envelope's `data`.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None, "deltas carry no seq");
    assert_eq!(kind.as_deref(), Some("assistant_delta"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["class"], "output");
    assert_eq!(v["data"]["text"], "hello ");

    // Tool-output delta: tagged by the call id so a client can attribute it.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("tool_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["data"]["call_id"], "toolu_5");
    assert_eq!(v["data"]["chunk"], "line of output\n");

    // Background-process delta: tagged by handle (TUI-bg-2), demuxed like a tool.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("bg_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["data"]["handle"], "bg#1");
    assert_eq!(v["data"]["chunk"], "tick 3\n");

    let _ = std::fs::remove_file(&log_path);
}
