//! Tests for `stream.rs`.

use super::*;
use futures_lite::StreamExt;
use std::time::Duration;

fn user(text: &str) -> Payload {
    Payload::User(vec![crate::log::ContentBlock::Text(text.into())])
}

/// One frame as `(class, kind, seq, data)` — what a client actually branches on.
#[derive(Debug, Clone)]
struct Seen {
    class: String,
    kind: String,
    seq: Option<u64>,
    data: Value,
}

/// Collect frames until the stream goes quiet.
async fn collect(
    stream: &mut Pin<Box<dyn Stream<Item = FrameEvent> + Send + Sync + 'static>>,
) -> Vec<Seen> {
    let mut seen = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), stream.next()).await
    {
        let frame: Value = serde_json::from_str(event.data()).expect("a frame envelope");
        seen.push(Seen {
            class: frame["class"].as_str().unwrap_or_default().to_string(),
            kind: frame["kind"].as_str().unwrap_or_default().to_string(),
            seq: frame["seq"].as_u64(),
            data: frame["data"].clone(),
        });
    }
    seen
}

/// The `?include=` grammar: a bare class means all of it, `class:topic` narrows, and an
/// unknown token is rejected rather than dropped.
#[test]
fn include_parses_classes_and_topics() {
    assert_eq!(Include::parse(None).unwrap(), Include::default());
    assert_eq!(
        Include::parse(Some("log")).unwrap(),
        Include {
            log: true,
            output: false,
            topics: vec![]
        }
    );
    assert_eq!(
        Include::parse(Some("state")).unwrap().topics,
        Topic::ALL.to_vec()
    );
    // A watch app: the gate and the bill, no token firehose.
    assert_eq!(
        Include::parse(Some("state:gate,state:usage")).unwrap(),
        Include {
            log: false,
            output: false,
            topics: vec![Topic::Gate, Topic::Usage],
        }
    );
    // Canonical order regardless of how it was asked for, so frames don't depend on
    // query spelling.
    assert_eq!(
        Include::parse(Some("state:usage,state:gate"))
            .unwrap()
            .topics,
        vec![Topic::Gate, Topic::Usage]
    );

    // The important one: a typo'd topic is an error. Ignoring it would yield a stream
    // that never delivers a gate, which looks exactly like a daemon that never gated.
    assert!(Include::parse(Some("state:gaet")).is_err());
    assert!(Include::parse(Some("logs")).is_err());
    assert!(Include::parse(Some("")).is_err());
}

/// The SSE fields a client switches on before parsing anything: `event:` is the frame's
/// kind, and `id:` is the resume cursor — set on the durable class and nowhere else, so
/// `EventSource`'s Last-Event-ID tracks `seq` and is never corrupted by an ephemeral frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_fields_carry_the_kind_and_the_cursor() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut stream = control_stream(Arc::clone(&shared), Include::default(), 0);
    shared.append(1, user("hello")).unwrap();
    shared.publish_delta(StreamDelta::AssistantText("hi".into()));

    let mut kinds = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), stream.next()).await
    {
        let frame: Value = serde_json::from_str(event.data()).unwrap();
        kinds.push((
            frame["class"].as_str().unwrap_or_default().to_string(),
            event.event_type().map(str::to_string),
            event.id().map(str::to_string),
        ));
    }

    let log = kinds
        .iter()
        .find(|(class, ..)| class == "log")
        .expect("a log frame");
    assert_eq!(log.1.as_deref(), Some("user"), "event: is the payload kind");
    assert_eq!(log.2.as_deref(), Some("0"), "id: is the seq");

    assert!(
        kinds
            .iter()
            .filter(|(class, ..)| class != "log")
            .all(|(_, _, id)| id.is_none()),
        "no ephemeral frame carries a cursor: {kinds:?}"
    );
}

/// A connection is opened with every subscribed topic's current value, so a client
/// renders without a separate `GET /state` call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_opens_with_a_state_snapshot() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut stream = control_stream(
        Arc::clone(&shared),
        Include::parse(Some("state")).unwrap(),
        0,
    );

    let seen = collect(&mut stream).await;
    assert_eq!(seen.len(), Topic::ALL.len(), "every subscribed topic");
    assert!(seen.iter().all(|f| f.class == "state"));
    // Frames are whole-topic values, and `null` is meaningful: no gate is open.
    let gate = seen
        .iter()
        .find(|f| f.kind == "gate")
        .expect("a gate frame");
    assert!(gate.data.is_null(), "null means cleared, not absent");
}

/// **`GET /state` ≡ the fold of every `state` frame since connect.** One definition, two
/// transports — and the property that lets the poll be deleted rather than merely
/// supplemented.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fold_of_state_frames_equals_get_state() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    tokio::spawn(watch_topics(Arc::clone(&shared)));
    let mut stream = control_stream(
        Arc::clone(&shared),
        Include::parse(Some("state")).unwrap(),
        0,
    );

    // Move several topics, through the same methods the routes call.
    shared.set_review_level(crate::policy::ReviewLevel::HumanOnly);
    shared.set_pace(Some(Duration::from_millis(1500)), Some(true));
    shared.notify("watch".into(), "cargo test finished".into(), None, None);

    // Fold every frame into a map, last write per topic winning.
    let mut folded = serde_json::Map::new();
    for frame in collect(&mut stream).await {
        assert_eq!(frame.class, "state");
        folded.insert(frame.kind, frame.data);
    }

    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    for topic in Topic::ALL {
        assert_eq!(
            folded.get(topic.as_str()),
            state.get(topic.as_str()),
            "topic {} differs between the fold and GET /state",
            topic.as_str()
        );
    }
    // `last_seq` is the log's cursor, not control state, so it is deliberately outside
    // the fold — the one `GET /state` key with no frame.
    assert!(state.get("last_seq").is_some());
    assert!(!folded.contains_key("last_seq"));
}

/// **Coherence**: no `state` frame is delivered before a `log` frame it depends on.
///
/// Asserted as a property over the trace rather than as a fixed frame order — pinning a
/// literal sequence would pin scheduling, and would break on changes that keep the
/// guarantee intact. Driven through the entangled pair from the design: the gate carries a
/// `reason` read back out of a `spot` entry, so a client seeing the gate first would hold
/// text quoting an entry it had not received.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_state_frame_arrives_before_the_log_it_reflects() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    tokio::spawn(watch_topics(Arc::clone(&shared)));

    // A backlog of durable history the connection must deliver first.
    for i in 0..20 {
        shared.append(1, user(&format!("entry {i}"))).unwrap();
    }

    // Open at the *start* of that history, the worst case for ordering: every state
    // frame's watermark sits at the far end of a replay the connection has not sent yet.
    let mut stream = control_stream(Arc::clone(&shared), Include::default(), 0);

    shared.set_review_level(crate::policy::ReviewLevel::HumanOnly);

    let seen = collect(&mut stream).await;
    let delivered_before_first_state = seen
        .iter()
        .take_while(|f| f.class != "state")
        .filter(|f| f.class == "log")
        .count();
    assert_eq!(
        delivered_before_first_state, 20,
        "state waits behind the log it reflects; got {seen:?}"
    );

    // And the log arrived in order, with `seq` on the durable class only.
    let log_seqs: Vec<u64> = seen
        .iter()
        .filter(|f| f.class == "log")
        .map(|f| f.seq.expect("log frames carry seq"))
        .collect();
    assert_eq!(log_seqs, (0..20).collect::<Vec<_>>());
    assert!(
        seen.iter()
            .filter(|f| f.class != "log")
            .all(|f| f.seq.is_none()),
        "only log frames carry the resume cursor"
    );
}

/// A client subscribed to `state` alone is not held behind a log it never receives.
///
/// Without this the watch-app case — `?include=state` — would wait forever on a cursor
/// that never advances, which is the degenerate reading of the coherence rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_only_subscribers_are_not_gated_on_the_log() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    tokio::spawn(watch_topics(Arc::clone(&shared)));
    for i in 0..5 {
        shared.append(1, user(&format!("entry {i}"))).unwrap();
    }

    let mut stream = control_stream(
        Arc::clone(&shared),
        Include::parse(Some("state:review_level")).unwrap(),
        0,
    );
    shared.set_review_level(crate::policy::ReviewLevel::Autonomous);

    let seen = collect(&mut stream).await;
    assert!(
        seen.iter()
            .all(|f| f.class == "state" && f.kind == "review_level"),
        "only the subscribed topic: {seen:?}"
    );
    assert_eq!(
        seen.last().expect("a frame").data,
        Value::from("autonomous"),
        "the latest value arrives despite an undelivered log"
    );
}

/// An update arriving while the connection is **parked in its wait** is delivered, not
/// consumed and dropped.
///
/// The wait's channel arms complete by receiving — cancel-safety covers only the losing
/// branches — so a discarded completion value is a real message gone. For state that
/// failure is a topic stuck stale until its next unrelated change: a gate appearing
/// while an idle client is parked would simply never render. The park matters: polling
/// to quiescence first is what routes the update through the wait rather than through
/// `absorb_state`'s `try_recv`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_arriving_while_parked_is_not_dropped() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    tokio::spawn(watch_topics(Arc::clone(&shared)));
    let mut stream = control_stream(
        Arc::clone(&shared),
        Include::parse(Some("state:review_level")).unwrap(),
        0,
    );

    // Drain the connect snapshot until quiet, leaving the stream parked in its wait.
    collect(&mut stream).await;

    shared.set_review_level(crate::policy::ReviewLevel::Autonomous);

    let seen = collect(&mut stream).await;
    assert_eq!(
        seen.last().map(|f| f.data.clone()),
        Some(Value::from("autonomous")),
        "the update that arrived while parked was delivered: {seen:?}"
    );
}

/// The same property for the ephemeral class: a delta arriving while parked is sent, not
/// silently dropped. `output` is lossy under backpressure (the ring drops oldest), but an
/// idle connection has no backpressure — losing one delta per park would be corruption in
/// the live tail, not load-shedding.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delta_arriving_while_parked_is_not_dropped() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut stream = control_stream(
        Arc::clone(&shared),
        Include::parse(Some("output")).unwrap(),
        0,
    );

    collect(&mut stream).await;

    shared.publish_delta(StreamDelta::AssistantText("while parked".into()));

    let seen = collect(&mut stream).await;
    assert_eq!(
        seen.first().map(|f| f.data["text"].clone()),
        Some(Value::from("while parked")),
        "the delta that arrived while parked was delivered: {seen:?}"
    );
}

/// Log frames are never delayed behind the ephemeral class — the hazard one socket
/// introduces that three did not, and the reason the drain order is part of the contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_durable_class_is_drained_first() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let mut stream = control_stream(
        Arc::clone(&shared),
        Include::parse(Some("log,output")).unwrap(),
        0,
    );

    // Publish deltas first, then append: the durable entry must still lead.
    for i in 0..5 {
        shared.publish_delta(StreamDelta::AssistantText(format!("chunk {i}")));
    }
    shared.append(1, user("committed")).unwrap();

    let seen = collect(&mut stream).await;
    let first = seen.first().expect("a frame");
    assert_eq!(first.class, "log", "durable first, got {seen:?}");
    assert!(
        seen.iter().any(|f| f.class == "output"),
        "the ephemeral class still arrives"
    );
}
