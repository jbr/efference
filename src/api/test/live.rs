//! Tests for over-the-wire async behaviour: the stream ceiling, interruption,
//! an empty stream's captured response headers, OpenRouter attribution, and
//! the refusal of a foreign-dialect turn list.

use super::*;

/// **The duration ceiling** (docs/transport.md): a stream that opens, trickles,
/// and then never finalizes is given up on rather than blocking its turn forever,
/// and the completion comes back marked exactly the way a mid-stream abort is —
/// record-only, retryable, with the reason on it. The server here holds the
/// connection open with an unterminated SSE body, which is the shape nothing
/// bounded before this: the retry policy applies only while *opening* a response.
///
/// One read loop serves both stream surfaces, so a turn and a fork peek are
/// bounded by the same knob.
#[tokio::test]
async fn a_stream_open_past_the_ceiling_is_marked_retryable() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // The server holds the connection open until the test says it is done, so the
    // *client's* ceiling is always what ends the read. A server that closed on its
    // own timer would race it, and an EOF already waiting on the socket wins over an
    // elapsed deadline — the assertion would then be about the wrong failure.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = std::sync::Arc::clone(&done);
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
        sock.flush().unwrap();
        // Never send the terminating chunk: the response is open and unfinalized,
        // which is the shape nothing bounded before this.
        sock.set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        while !held.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = sock.read(&mut buf);
        }
    });

    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    };
    let client = Client::new(endpoint, "m", "sys", None)
        .with_stream_ceiling(std::time::Duration::from_millis(200));
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the ceiling is not an open failure — it assembles a partial");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("ceiling"), "{why}");
    assert!(why.contains("stream-ceiling"), "names the knob: {why}");
    server.join().unwrap();
}

/// The operator's interrupt ends a read in flight (docs/control-channel.md): the
/// server holds an unfinalized stream open, the notifier fires, and `complete`
/// returns at once with the partial marked `Aborted` — record-only like any
/// aborted attempt, but a class the retry loops do not retry. The notifier is
/// registered before the first read, so a press that lands between events is
/// not lost to `notify_waiters`' no-permit semantics.
#[tokio::test]
async fn an_interrupt_ends_the_read_and_marks_the_partial_aborted() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = std::sync::Arc::clone(&done);
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
        sock.flush().unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        while !held.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = sock.read(&mut buf);
        }
    });

    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    };
    let interrupt = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut client = Client::new(endpoint, "m", "sys", None)
        .with_stream_ceiling(std::time::Duration::from_secs(30));
    client.set_abort(std::sync::Arc::clone(&interrupt));
    let presser = std::sync::Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        presser.notify_waiters();
    });
    let started = std::time::Instant::now();
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an interrupt is not an open failure — it assembles a partial");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the read ended on the interrupt, not the ceiling"
    );
    assert!(completion.operator_abort());
    assert_eq!(completion.failure_class, FailureClass::Aborted);
    assert!(completion.terminal_failure().is_none());
    let why = completion.upstream_failure().unwrap();
    assert!(why.contains("interrupt"), "{why}");
    assert!(
        matches!(
            &completion.assistant,
            Payload::Assistant {
                upstream_failure: Some(_),
                ..
            }
        ),
        "the partial is record-only"
    );
    server.join().unwrap();
}

/// End to end over a socket: the diagnostics come off the *response*, so the
/// hook is in `complete` — the status handler cannot serve this path, an empty
/// stream being a 200 it early-returns on. The server here answers exactly as a
/// gateway fronting a slow upstream does: 200, an `x-generation-id`, a
/// `message_start` with all-zero usage, `[DONE]`, nothing else.
#[tokio::test]
async fn an_empty_stream_over_the_wire_captures_the_response_headers() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf);
        let body = "event: message_start\ndata: {\"message\":{\"id\":\"gen-abc123\",\
                    \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                    event: data\ndata: [DONE]\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 x-generation-id: gen-abc123\r\nset-cookie: session=nope\r\n\
                 content-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        sock.flush().unwrap();
    });

    let endpoint = Endpoint {
        provider: Provider::OpenRouter,
        url: Some(format!("http://{addr}/api/v1/messages")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    };
    let client = Client::new(endpoint, "m", "sys", None);
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an empty stream assembles a record-only entry, not an error");
    server.join().unwrap();

    let Payload::Assistant {
        upstream_failure: Some(failure),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.headers.get("x-generation-id").map(String::as_str),
        Some("gen-abc123")
    );
    // Off the allowlist, so it never reaches the log (docs/env.md).
    assert!(!stream.headers.contains_key("set-cookie"));
    assert!(stream.frames.contains("data: [DONE]"), "{stream:?}");
}

/// The ceiling is a `[transport]` knob with a generous default, not a hardcoded
/// number: the right value is workload-dependent (docs/transport.md).
#[test]
fn the_stream_ceiling_defaults_generously_and_is_operator_settable() {
    assert_eq!(
        crate::config::TransportLimits::default().stream_ceiling,
        DEFAULT_STREAM_CEILING
    );
    // Comfortably past the widest model call in this repo's corpus (363s).
    assert!(DEFAULT_STREAM_CEILING >= std::time::Duration::from_secs(3600));
}

/// The attribution headers ride an OpenRouter inference POST and no other
/// (docs/transport.md). Asserted over a socket rather than off
/// `Provider::attribution` alone, because the thing that can break is the wiring:
/// `with_attribution` is applied on two conn-building paths and would be silently
/// absent from either.
///
/// The negative half is the point. `HTTP-Referer` is a header hosts act on with
/// its RFC 9110 meaning, so an Anthropic turn must carry none of these — while
/// still carrying the `User-Agent`, which every host is meant to see.
#[tokio::test]
async fn attribution_rides_openrouter_turns_and_nothing_else() {
    use std::io::{Read, Write};

    /// The request head of one round trip against a canned reply. `sse` picks
    /// the path: an empty stream for `complete`, a usage-only body for
    /// `keepalive`.
    async fn request_head(provider: Provider, sse: bool) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut request = String::new();
            let mut buf = [0u8; 16384];
            while !request.contains("\r\n\r\n") {
                let read = sock.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
            }
            let (content_type, body) = if sse {
                (
                    "text/event-stream",
                    "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
                     \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                     event: data\ndata: [DONE]\n\n",
                )
            } else {
                (
                    "application/json",
                    "{\"id\":\"msg-1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\
                     \"output_tokens\":0}}",
                )
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
            request
        });

        let endpoint = Endpoint {
            provider,
            url: Some(format!("http://{addr}/v1/messages")),
            auth_header: "x-api-key".into(),
            auth_value: "k".into(),
            session_id: None,
            extra_headers: Vec::new(),
            base: None,
        };
        let client = Client::new(endpoint, "m", "sys", None);
        if sse {
            client
                .complete(&[], &[], ToolChoice::auto(), |_| {})
                .await
                .expect("the canned stream assembles");
        } else {
            client
                .keepalive(&[], &[], ToolChoice::auto())
                .await
                .expect("the canned body parses");
        }
        let request = server.join().unwrap();
        request
            .split_once("\r\n\r\n")
            .map_or(request.clone(), |(head, _)| head.to_string())
            .to_ascii_lowercase()
    }

    // The `User-Agent` is the one header every request carries, on every path.
    let expected_ua = format!("user-agent: {}", crate::http::user_agent()).to_ascii_lowercase();

    for sse in [true, false] {
        let openrouter = request_head(Provider::OpenRouter, sse).await;
        for header in [
            "http-referer: https://efference.net",
            "x-openrouter-title: efference",
            "x-openrouter-categories: cli-agent,programming-app",
        ] {
            assert!(
                openrouter.contains(header),
                "missing {header}: {openrouter}"
            );
        }
        assert!(openrouter.contains(&expected_ua), "{openrouter}");

        let anthropic = request_head(Provider::Anthropic, sse).await;
        assert!(!anthropic.contains("referer"), "{anthropic}");
        assert!(!anthropic.contains("x-openrouter"), "{anthropic}");
        assert!(anthropic.contains(&expected_ua), "{anthropic}");
    }
}

/// The duration ceiling on the Responses dialect: a stream that delivered partial
/// text and then stayed open is cut at the ceiling, the partial kept record-only
/// with a transient class, so the abort schedule retries it (docs/transport.md).
#[tokio::test]
async fn a_responses_stream_past_the_ceiling_keeps_its_partial_as_transient() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = std::sync::Arc::clone(&done);
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf);
        let events = "event: response.created\ndata: {\"type\":\"response.created\",\
                      \"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\",\"output\":[]}}\n\n\
                      event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\
                      \"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\
                      \"status\":\"in_progress\",\"content\":[]}}\n\n\
                      event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\
                      \"output_index\":0,\"content_index\":0,\"delta\":\"half an\"}\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 transfer-encoding: chunked\r\n\r\n{:x}\r\n{events}\r\n",
                events.len()
            )
            .as_bytes(),
        )
        .unwrap();
        sock.flush().unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        while !held.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = sock.read(&mut buf);
        }
    });
    let endpoint = Endpoint {
        provider: Provider::OpenAI,
        url: Some(format!("http://{addr}/v1/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys", None)
        .with_stream_ceiling(std::time::Duration::from_millis(200));
    let mut streamed = Vec::new();
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |s| streamed.push(s))
        .await
        .expect("the ceiling assembles a partial");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    let why = completion.upstream_failure().expect("marked");
    assert!(why.contains("stream-ceiling"), "{why}");
    assert_eq!(completion.failure_class, FailureClass::Transient);
    assert!(
        streamed.iter().any(|s| matches!(s, Streamed::Text { .. })),
        "the partial text streamed before the cut"
    );
}

/// The dialect guard sits before the encoder in `complete` (docs/transport.md): a
/// turn list recorded in the other dialect is refused with the conflict named, and
/// no request leaves — the endpoint here has nothing listening, so a request would
/// fail with a connection error instead of this message.
#[tokio::test]
async fn a_foreign_dialect_turn_list_never_becomes_a_request() {
    use crate::project::WireTurn;
    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some("http://127.0.0.1:1/v1/messages".into()),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        base: None,
    };
    let client = Client::new(endpoint, "m", "sys", None);
    let turns = vec![
        WireTurn::text("hello"),
        WireTurn::Assistant(AssistantOutput::Responses(vec![
            crate::log::ResponsesOutputItem::FunctionCall {
                id: None,
                call_id: CallId("call_1".into()),
                name: "bash".into(),
                arguments: "{}".into(),
                status: None,
            },
        ])),
    ];
    let err = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .expect_err("a Responses history must not encode onto the Messages wire");
    let text = err.to_string();
    assert!(text.contains("recorded in the Responses dialect"), "{text}");
    assert!(text.contains("speaks Messages"), "{text}");
}
