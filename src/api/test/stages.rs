//! Tests for the request-stage seam (`api/stages.rs`, docs/log-and-projection.md):
//! the stages a request passes, in order, for a normal request, a retried one, an
//! interrupted one, a cancelled one, and two that overlap.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::log::{RequestOrigin, RequestStage, RequestStageKind};

#[derive(Default)]
struct Recorder {
    next: AtomicU64,
    stages: Mutex<Vec<RequestStage>>,
}

impl StageRecorder for Recorder {
    fn mint(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }
    fn record(&self, stage: RequestStage) {
        self.stages.lock().unwrap().push(stage);
    }
}

impl Recorder {
    /// The kinds recorded for one request, in order.
    fn kinds(&self, request: u64) -> Vec<RequestStageKind> {
        self.stages
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.request == request)
            .map(|s| s.stage.clone())
            .collect()
    }
}

const FINALIZED: &str = "event: message_start\ndata: {\"type\":\"message_start\",\
    \"message\":{\"id\":\"gen-1\",\"model\":\"m\",\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n\
    event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n\
    event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

fn ok_response() -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\
         content-length: {}\r\n\r\n{FINALIZED}",
        FINALIZED.len()
    )
}

fn unavailable() -> String {
    let body = "{\"error\":{\"message\":\"overloaded\"}}";
    format!(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\n\
         connection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// A server that answers each accepted connection with the next scripted response.
fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for response in responses {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 16384];
            let _ = sock.read(&mut buf);
            sock.write_all(response.as_bytes()).unwrap();
            let _ = sock.flush();
        }
    });
    (url, server)
}

fn endpoint(url: String) -> Endpoint {
    Endpoint {
        provider: Provider::Anthropic,
        url: Some(url),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    }
}

fn client(url: String, recorder: &Arc<Recorder>) -> Client {
    let mut client = Client::new(endpoint(url), "m", "sys", None);
    client.set_stage_sink(Arc::clone(recorder) as StageSink);
    client
}

/// A request that completes records sent, first byte, ended.
#[tokio::test]
async fn a_normal_request_records_sent_first_byte_ended() {
    let (url, server) = serve(vec![ok_response()]);
    let recorder = Arc::new(Recorder::default());
    client(url, &recorder)
        .with_origin(RequestOrigin::Turn)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::FirstByte,
            RequestStageKind::Ended
        ]
    );
    assert!(
        recorder
            .stages
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.origin == RequestOrigin::Turn)
    );
    // The ended stage keeps the provider's own word for the finish; no other
    // stage carries one.
    let finishes: Vec<_> = recorder
        .stages
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.finish.clone())
        .collect();
    assert_eq!(finishes, [None, None, Some("end_turn".to_string())]);
}

/// A refused attempt the transport retries is recorded between sent and first
/// byte, and the request still ends once.
#[tokio::test]
async fn a_retried_request_records_the_refused_attempt() {
    let (url, server) = serve(vec![unavailable(), ok_response()]);
    let recorder = Arc::new(Recorder::default());
    client(url, &recorder)
        .with_origin(RequestOrigin::Peek)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::AttemptFailed {
                attempt: 1,
                status: 503
            },
            RequestStageKind::FirstByte,
            RequestStageKind::Ended
        ]
    );
}

/// The operator's interrupt closes the request as `failed: interrupted`.
#[tokio::test]
async fn an_interrupted_request_records_failed_interrupted() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let done = Arc::new(AtomicBool::new(false));
    let held = Arc::clone(&done);
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf);
        let event = "event: message_start\ndata: {\"message\":{\"model\":\"m\",\
                     \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 transfer-encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n",
                event.len()
            )
            .as_bytes(),
        )
        .unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        while !held.load(Ordering::SeqCst) {
            let _ = sock.read(&mut buf);
        }
    });
    let recorder = Arc::new(Recorder::default());
    let interrupt = Arc::new(tokio::sync::Notify::new());
    let mut client = client(url, &recorder);
    client.set_abort(Arc::clone(&interrupt));
    let presser = Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        presser.notify_waiters();
    });
    client
        .with_origin(RequestOrigin::Turn)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    done.store(true, Ordering::SeqCst);
    server.join().unwrap();

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::FirstByte,
            RequestStageKind::Failed {
                reason: "interrupted".into()
            }
        ]
    );
}

/// A request whose future is dropped before it finishes does not stay open.
#[tokio::test]
async fn a_cancelled_request_closes_as_interrupted() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let done = Arc::new(AtomicBool::new(false));
    let held = Arc::clone(&done);
    let server = std::thread::spawn(move || {
        // Accept and say nothing until the client gives up.
        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut buf = [0u8; 8192];
        while !held.load(Ordering::SeqCst) {
            let _ = sock.read(&mut buf);
        }
    });
    let recorder = Arc::new(Recorder::default());
    let client = client(url, &recorder).with_origin(RequestOrigin::Title);
    let cancelled = tokio::time::timeout(
        Duration::from_millis(200),
        client.complete(&[], &[], ToolChoice::auto(), |_| {}),
    )
    .await;
    assert!(cancelled.is_err(), "the server never answers");
    done.store(true, Ordering::SeqCst);
    server.join().unwrap();

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::Failed {
                reason: "interrupted".into()
            }
        ]
    );
}

/// Two requests in flight at once keep their own ids and origins; their stages
/// interleave in the order they happened.
#[tokio::test]
async fn concurrent_requests_interleave_by_id() {
    let (url, server) = serve(vec![ok_response(), ok_response()]);
    let recorder = Arc::new(Recorder::default());
    let base = client(url, &recorder);
    let peek = base.with_origin(RequestOrigin::Peek);
    let title = base.with_origin(RequestOrigin::Title);
    let (a, b) = tokio::join!(
        peek.complete(&[], &[], ToolChoice::auto(), |_| {}),
        title.complete(&[], &[], ToolChoice::auto(), |_| {}),
    );
    a.unwrap();
    b.unwrap();
    server.join().unwrap();

    let stages = recorder.stages.lock().unwrap().clone();
    assert_eq!(stages.len(), 6);
    for request in [0, 1] {
        let own: Vec<_> = stages.iter().filter(|s| s.request == request).collect();
        assert_eq!(own.len(), 3, "each request records its own three stages");
        assert!(own.iter().all(|s| s.origin == own[0].origin));
        assert_eq!(own[2].stage, RequestStageKind::Ended);
    }
    let origins: std::collections::HashSet<_> = stages.iter().map(|s| s.origin).collect();
    assert_eq!(origins.len(), 2);
}
