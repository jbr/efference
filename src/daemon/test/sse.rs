//! Tests for `sse.rs`.

use super::*;

/// `GET /events` replays the pre-existing log (id == seq) and then follows new
/// appends live — including an append that lands *after* the client has caught
/// up and is awaiting, which is the lost-wakeup path the tail is built to
/// survive (the `Notified` is armed before the catch-up read).
///
/// Multi-thread flavor: an SSE body streams server-write ↔ client-read
/// concurrently, which a single-threaded test runtime can deadlock (the real
/// daemon runs on the multi-thread runtime).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_replays_then_tails_live() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-sse-test.jsonl");
    let socket = dir.join("efference-sse-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    // Two entries present before any client connects (the replay set).
    shared.append(1, user("first")).unwrap();
    shared.append(2, user("second")).unwrap();

    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    let mut events = client.events(Some(0)).await.expect("open events");

    // Replay: the two pre-existing entries, in order, id == seq, typed `user`.
    let (id, kind, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("0"));
    assert_eq!(kind.as_deref(), Some("user"));
    assert!(data.contains("first"));
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("1"));
    assert!(data.contains("second"));

    // Live tail: append once the client is caught up and parked on the wait.
    shared.append(3, user("third")).unwrap();
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("2"));
    assert!(data.contains("third"));

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}

/// `GET /stream` carries ephemeral deltas published after a client subscribes:
/// assistant prose (`assistant_delta`) and tool output (`tool_output`), each
/// typed, JSON-bodied, and crucially **id-less** — they're pre-commit and have
/// no `seq`, which is the contract that keeps them off the durable `/events`
/// cursor. The handler subscribes synchronously while building the response, so
/// a publish after `stream()` resolves can't race ahead of the subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_carries_ephemeral_deltas() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-stream-test.jsonl");
    let socket = dir.join("efference-stream-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    let mut stream = client.stream().await.expect("open stream");

    // Published only after the subscription exists (the handler subscribed
    // before returning the response headers `stream()` just awaited).
    shared.publish_delta(StreamDelta::AssistantText("hello ".into()));
    shared.publish_delta(StreamDelta::ToolOutput {
        call_id: "toolu_5".into(),
        chunk: "line of output\n".into(),
    });
    shared.publish_delta(StreamDelta::BgOutput {
        handle: "bg#1".into(),
        chunk: "tick 3\n".into(),
    });

    // Prose delta: typed, id-less, text in a JSON body.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None, "deltas carry no seq");
    assert_eq!(kind.as_deref(), Some("assistant_delta"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["text"], "hello ");

    // Tool-output delta: tagged by the call id so a client can attribute it.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("tool_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["call_id"], "toolu_5");
    assert_eq!(v["chunk"], "line of output\n");

    // Background-process delta: tagged by handle (TUI-bg-2), demuxed like a tool.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("bg_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["handle"], "bg#1");
    assert_eq!(v["chunk"], "tick 3\n");

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}
