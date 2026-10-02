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

    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
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

/// An idle `/events` stream is never entirely silent: whenever the heartbeat interval
/// passes without a frame, the daemon sends an SSE **comment** (a `:` line) — the one
/// keepalive shape a client's event parser discards entirely, which is why this test
/// must read raw body bytes rather than decoded events (the invisibility is the point).
/// The heartbeat is what makes a `?since` past the head — served as a wait, not an
/// error — distinguishable from a dead connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_stream_heartbeats_with_comments() {
    use futures_lite::AsyncReadExt;

    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let shared_for_init = Arc::clone(&shared);
    let mut handler = (
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared_for_init);
            async move { info.with_shared_state(shared) }
        }),
        // The interval parameter exists for this test: short enough to observe, long
        // enough that the snapshot frame reliably precedes the first heartbeat.
        super::super::http::control_routes_with_heartbeat(Duration::from_millis(50)),
    );
    let context = trillium_testing::init(&mut handler).await;
    let raw = trillium_client::Client::new(
        trillium_testing::ServerConnector::new(handler).with_context(context),
    )
    .with_base("http://localhost/");

    // `Accept` names the stream by name, as `into_sse` does for the real client:
    // the `Sse` handler passes anything else through to the router's 404.
    let mut conn = raw
        .get("/v1/events?include=state:gate")
        .with_request_header(trillium::KnownHeaderName::Accept, "text/event-stream")
        .await
        .expect("open events");
    let mut body = Vec::new();
    let mut buf = [0u8; 1024];
    tokio::time::timeout(Duration::from_secs(5), async {
        // A comment line starts with `:`; field lines start with `event`/`data`/`id`,
        // and data is single-line JSON, so `\n:` can only be a comment.
        while !body.windows(2).any(|w| w == b"\n:") {
            let n = conn
                .response_body()
                .read(&mut buf)
                .await
                .expect("read body");
            assert_ne!(n, 0, "the stream ended before a heartbeat");
            body.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .expect("a heartbeat comment within the timeout");

    // The stream is still a working event stream around the comment: the connect
    // snapshot's gate frame arrived as a normal event.
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: gate"), "{text}");
}

/// `?include=output` carries ephemeral deltas published after a client subscribes:
/// assistant prose (`assistant_delta`), tool output (`tool_output`), and process
/// output (`process_output`), each typed by the frame's `kind`, payloaded under the
/// envelope's `data`, and crucially **id-less** — they're pre-commit and have no
/// `seq`, which is the contract that keeps them off the durable resume cursor. The
/// handler subscribes synchronously while building the response, so a publish after
/// `events()` resolves can't race ahead of the subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_output_class_carries_ephemeral_deltas() {
    let log_path = std::env::temp_dir().join("efference-stream-test.jsonl");
    let _ = std::fs::remove_file(&log_path);

    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("create log"),
    );
    let client = test_client(&shared).await;
    let mut stream = client
        .events(None, Some("output"))
        .await
        .expect("open events");

    // Published only after the subscription exists (the handler subscribed
    // before returning the response headers `events()` just awaited).
    shared.publish_delta(StreamDelta::AssistantText {
        index: 0,
        text: "hello ".into(),
    });
    shared.publish_delta(StreamDelta::ToolOutput {
        call_id: "toolu_5".into(),
        chunk: "line of output\n".into(),
    });
    shared.publish_delta(StreamDelta::ProcessOutput {
        id: 1,
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

    // Background-process delta: tagged by the bare process id, demuxed like a tool.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("process_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["data"]["id"], 1);
    assert_eq!(v["data"]["chunk"], "tick 3\n");

    let _ = std::fs::remove_file(&log_path);
}

/// `?presence=off` is how a non-human consumer keeps its connection out of the
/// watched count (docs/web-push.md). A value that is neither spelling is a typed 400,
/// on the same rule as an unknown `?include=` token: a client that meant to opt out
/// and misspelled it would otherwise be counted as a watcher and suppress the very
/// notifications it connected to observe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unspelled_presence_value_is_a_typed_error() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let app = test_server(&shared).await;

    let conn = app.get("/v1/events?presence=maybe").await;
    conn.assert_status(400);
    conn.assert_json_body_with(|body: &Value| {
        assert_eq!(body["code"], "bad_request");
        assert!(
            body["message"].as_str().unwrap().contains("presence"),
            "names the parameter: {body}"
        );
    });
}
