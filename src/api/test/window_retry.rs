//! Tests for the halve-and-retry on a window overflow refused inside a 200
//! stream (docs/transport.md), over a loopback server that answers each request
//! from a script.

use super::*;
use crate::project::WireTurn;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// The refusal as DeepInfra sends it through OpenRouter: an `error` event on a stream
/// that opened 200, with no error type, so the classifier calls it transient.
const OVERFLOW_EVENT: &str = "event: error\ndata: {\"error\":{\"message\":\"This model's maximum \
    context length is 131072 tokens. However, you requested 65536 output tokens and your prompt \
    contains at least 65537 input tokens, for a total of at least 131073 tokens.\",\
    \"code\":400}}\n\n";

/// The same refusal named a terminal type, which is how the classifier marks it
/// terminal (`api/failure.rs`).
const TYPED_OVERFLOW_EVENT: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"This model's maximum \
    context length is 131072 tokens. However, you requested 65536 output tokens and your prompt \
    contains at least 65537 input tokens.\"}}\n\n";

/// A stream that finalizes with one line of text.
const ANSWER: &str = "event: message_start\n\
    data: {\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n\
    event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
    event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n\
    event: content_block_stop\ndata: {\"index\":0}\n\n\
    event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
    event: message_stop\ndata: {}\n\n";

/// A server that answers its Nth POST with `script[N]` (200, event-stream) and records
/// the `max_tokens` each request carried. It stops at the first non-POST, which is
/// how the test closes it.
fn serve(
    script: Vec<&'static str>,
) -> (
    std::net::SocketAddr,
    Arc<Mutex<Vec<u64>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let handle = std::thread::spawn(move || {
        for body in script {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buf = [0u8; 8192];
            let body_at = loop {
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            if !request.starts_with(b"POST") {
                return;
            }
            let head = String::from_utf8_lossy(&request[..body_at]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|n| n.trim().parse().ok())
                .unwrap_or(0);
            while request.len() < body_at + length {
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let sent: serde_json::Value = serde_json::from_slice(&request[body_at..]).unwrap();
            record
                .lock()
                .unwrap()
                .push(sent["max_tokens"].as_u64().unwrap());
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        }
    });
    (addr, seen, handle)
}

/// A Messages client against `addr` with a 131072-token window and a 65536 output cap.
fn client(addr: std::net::SocketAddr) -> Client {
    let endpoint = Endpoint {
        provider: Provider::OpenRouter,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    Client::new(endpoint, "m", "sys", None)
        .with_max_output(65_536)
        .with_context_window(Some(131_072))
}

/// A refusal inside a 200 stream halves the reservation and resends, as the 400 at
/// open does, whether or not the refusal names a terminal type: the answer to the
/// second request is the completion, and the refused attempt leaves nothing behind.
#[tokio::test]
async fn an_in_stream_window_overflow_is_resent_at_half_the_reservation() {
    let (addr, seen, server) = serve(vec![OVERFLOW_EVENT, ANSWER]);
    let client = client(addr);
    let turns = [WireTurn::text("hello")];
    let mut deltas = 0;
    let completion = client
        .complete(&turns, &[], ToolChoice::auto(), |_| deltas += 1)
        .await
        .expect("the second request completes");
    let _ = std::net::TcpStream::connect(addr);
    server.join().unwrap();
    assert_eq!(*seen.lock().unwrap(), [65_536, 32_768]);
    assert!(completion.upstream_failure().is_none());
    assert_eq!(deltas, 1, "only the answer streamed");
}

/// The halving runs down to the floor and then stops: the last refusal is the
/// completion, terminal, with the provider's words.
#[tokio::test]
async fn an_in_stream_window_overflow_stops_at_the_floor() {
    let (addr, seen, server) = serve(vec![TYPED_OVERFLOW_EVENT; 7]);
    let client = client(addr);
    let turns = [WireTurn::text("hello")];
    let completion = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an in-stream refusal is a marked completion");
    let _ = std::net::TcpStream::connect(addr);
    server.join().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [65_536, 32_768, 16_384, 8_192, 4_096, 2_048, 1_024]
    );
    let why = completion
        .terminal_failure()
        .expect("terminal at the floor");
    assert!(why.contains("maximum context length"), "{why}");
}
