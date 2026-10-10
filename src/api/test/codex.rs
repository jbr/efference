//! Tests for Codex-specific wire behaviour: turn-routing-token replay and
//! drop, plan windows read and retained, plan windows ignored off the
//! backend, and the Codex request headers.

use super::*;
use crate::log::RequestOrigin;

/// The Codex backend's turn-routing contract over a socket (docs/transport.md):
/// the first request of a turn carries no token, the token the backend issues
/// on that response rides every later request of the turn unchanged (a second
/// issued value does not replace it), and a new turn starts without one. The
/// model hint rides every request. Three connections, each closed by the server,
/// so the pool opens a fresh one per request and the mock sees each head whole.
#[tokio::test]
async fn codex_turn_routing_token_is_replayed_within_a_turn_and_dropped_at_the_next() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let issued = [Some("tok-one"), Some("tok-two"), None];
        let mut heads = Vec::new();
        for token in issued {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let n = sock.read(&mut buf).unwrap();
            heads.push(String::from_utf8_lossy(&buf[..n]).to_string());
            let body = "event: response.completed\ndata: {\"type\":\"response.completed\",\
                        \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
                        \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
            let token_line =
                token.map_or(String::new(), |t| format!("x-codex-turn-state: {t}\r\n"));
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n{token_line}\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
        heads
    });

    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-5.6-sol", "sys", None);
    assert_eq!(client.turn_routing_token(), None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(client.turn_routing_token().as_deref(), Some("tok-one"));
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        client.turn_routing_token().as_deref(),
        Some("tok-one"),
        "the first issued token holds for the whole turn"
    );
    client.begin_turn();
    assert_eq!(client.turn_routing_token(), None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let heads = server.join().unwrap();

    let head = |i: usize| {
        heads[i]
            .split("\r\n\r\n")
            .next()
            .unwrap()
            .to_ascii_lowercase()
    };
    for i in 0..3 {
        assert!(
            head(i).contains("x-codex-routing-hint: model=gpt-5.6-sol"),
            "request {i} names the model for the router"
        );
    }
    assert!(
        !head(0).contains("x-codex-turn-state"),
        "a turn's first request carries no token"
    );
    assert!(
        head(1).contains("x-codex-turn-state: tok-one"),
        "the second request replays it"
    );
    assert!(
        !head(2).contains("x-codex-turn-state"),
        "a new turn starts without one"
    );
}

/// A client outside any turn (the spotter's) never holds a token, whatever the
/// backend sends.
#[test]
fn a_client_without_turn_routing_holds_no_token() {
    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some("http://127.0.0.1:1/x".into()),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys", None).without_turn_routing();
    client.begin_turn();
    assert_eq!(client.turn_routing_token(), None);
}

/// The Codex backend's plan windows are read off every response, a 429 included,
/// and a response that carries none leaves the last snapshot standing
/// (docs/transport.md). Three connections: a 200 with both windows, a 200 with
/// none, and a 429 that moves the primary window.
#[tokio::test]
async fn codex_plan_windows_are_read_off_every_response_and_retained_when_absent() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let responses = [
            (
                "200 OK",
                "x-codex-plan-type: plus\r\nx-codex-primary-used-percent: 12\r\n\
                 x-codex-primary-window-minutes: 300\r\nx-codex-primary-reset-at: 1788677725\r\n\
                 x-codex-secondary-used-percent: 19\r\nx-codex-secondary-window-minutes: 10080\r\n\
                 x-codex-secondary-reset-at: 1789180313\r\n\
                 x-codex-spark-primary-used-percent: 45\r\nx-codex-spark-limit-name: spark\r\n",
            ),
            ("200 OK", ""),
            (
                "429 Too Many Requests",
                "x-codex-plan-type: plus\r\nx-codex-primary-used-percent: 100\r\n\
                 x-codex-primary-window-minutes: 300\r\nx-codex-primary-reset-at: 1788744444\r\n\
                 x-codex-active-limit: codex-spark\r\nx-codex-spark-primary-used-percent: 100\r\n",
            ),
        ];
        for (status, plan_headers) in responses {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).unwrap();
            let body = if status.starts_with("200") {
                "event: response.completed\ndata: {\"type\":\"response.completed\",\
                 \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
                 \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
                    .to_string()
            } else {
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":15805}}"#.to_string()
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\n{plan_headers}\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
    });

    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-5.6-sol", "sys", None);
    let plan = client.plan_state();
    assert!(plan.lock().unwrap().is_none());

    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let first = plan
        .lock()
        .unwrap()
        .clone()
        .expect("read off the first response");
    assert_eq!(first.plan_type.as_deref(), Some("plus"));
    let ordinary = &first.limits["codex"];
    let primary = ordinary.primary.as_ref().unwrap();
    assert_eq!(primary.used_percent, 12.0);
    assert_eq!(primary.window_minutes, Some(300));
    assert_eq!(primary.resets_at, Some(1_788_677_725_000));
    assert_eq!(ordinary.secondary.as_ref().unwrap().used_percent, 19.0);
    assert_eq!(first.limits["codex_spark"].name.as_deref(), Some("spark"));
    assert_eq!(
        first.limits["codex_spark"]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        45.0
    );

    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        plan.lock().unwrap().clone(),
        Some(first.clone()),
        "a response without plan headers retains the last snapshot"
    );

    let refused = client.complete(&[], &[], ToolChoice::auto(), |_| {}).await;
    assert!(refused.is_err(), "the terminal 429 is not retried");
    server.join().unwrap();
    let last = plan.lock().unwrap().clone().unwrap();
    assert_eq!(last.active.as_deref(), Some("codex_spark"));
    let ordinary = &last.limits["codex"];
    assert_eq!(ordinary.primary.as_ref().unwrap().used_percent, 100.0);
    assert_eq!(
        ordinary.primary.as_ref().unwrap().resets_at,
        Some(1_788_744_444_000)
    );
    assert!(
        ordinary.secondary.is_none(),
        "a response naming only one window replaces the whole snapshot"
    );
}

/// A client on a provider without plan headers never fills the cell, whatever
/// the response says.
#[tokio::test]
async fn plan_windows_are_ignored_off_the_codex_backend() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf);
        let body = "event: message_start\ndata: {\"message\":{\"id\":\"gen-1\",\
                    \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n\
                    event: data\ndata: [DONE]\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 x-codex-plan-type: plus\r\nx-codex-primary-used-percent: 50\r\n\
                 connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    });
    let endpoint = Endpoint {
        provider: Provider::OpenRouter,
        url: Some(format!("http://{addr}/api/v1/messages")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "m", "sys", None);
    let _ = client.complete(&[], &[], ToolChoice::auto(), |_| {}).await;
    server.join().unwrap();
    assert!(client.plan_state().lock().unwrap().is_none());
}

/// The Responses HTTP boundary over a socket (docs/transport.md): a Codex request
/// carries the bearer token, the account header the endpoint resolved, and the
/// originator attribution, and no `anthropic-version` — that header belongs to
/// the Messages dialect, not to a provider.
#[tokio::test]
async fn a_codex_request_carries_its_headers_and_no_anthropic_version() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let n = sock.read(&mut buf).unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = "event: response.completed\ndata: {\"type\":\"response.completed\",\
                    \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
                    \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        head
    });
    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer tok".into(),
        session_id: None,
        extra_headers: vec![("ChatGPT-Account-ID".into(), "acct-1".into())],
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-5.6-luna", "sys", None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("a stream with no content-type still assembles on the Codex arm");
    let head = server.join().unwrap().to_ascii_lowercase();
    let head = head.split("\r\n\r\n").next().unwrap().to_string();
    assert!(head.contains("authorization: bearer tok"), "{head}");
    assert!(head.contains("chatgpt-account-id: acct-1"), "{head}");
    assert!(head.contains("originator: efference"), "{head}");
    assert!(!head.contains("anthropic-version"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");
}

/// The Responses keepalive (docs/transport.md) is the turn's own envelope with the
/// output capped at the backend's minimum, which the Codex profile otherwise omits;
/// a turn still sends no cap. Its usage comes back as the ping's, cached read
/// included.
#[tokio::test]
async fn codex_keepalive_caps_output_where_a_turn_sends_no_cap() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut sock, _) = listener.accept().unwrap();
            // Head and body can arrive in separate reads: read to the body's length.
            let mut request = Vec::new();
            let mut buf = vec![0u8; 65536];
            loop {
                let n = sock.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if n == 0 || body.len() >= length {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&request).to_string());
            let body = "event: response.completed\ndata: {\"type\":\"response.completed\",\
                        \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
                        \"usage\":{\"input_tokens\":1000,\"input_tokens_details\":\
                        {\"cached_tokens\":900},\"output_tokens\":16}}}\n\n";
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
        requests
    });

    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-6.1-sol", "sys", None);
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let ping = client
        .keepalive(RequestOrigin::Keepalive, &[], &[], ToolChoice::auto())
        .await
        .unwrap();
    assert_eq!(ping.usage.cache_read_input_tokens, 900);
    assert_eq!(ping.usage.output_tokens, 16);

    let requests = server.join().unwrap();
    assert!(
        !requests[0].contains("max_output_tokens"),
        "a turn sends no cap"
    );
    assert!(
        requests[1].contains(r#""max_output_tokens":16"#),
        "the ping caps its output"
    );
}

/// The triage primer (backlog/separate-triage-cold-prefix.md) replays the turn's own
/// envelope with the backend-minimum cap: its input bytes match the turn's exactly,
/// so the Codex backend resumes where the main request ended. Its `tool_choice`
/// differs on purpose — forced `retain`, matching the peek's head rather than the
/// turn's `auto`, since the backend keys the resume on it.
#[tokio::test]
async fn codex_primer_replays_the_turn_envelope_capped() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut sock, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = vec![0u8; 65536];
            loop {
                let n = sock.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if n == 0 || body.len() >= length {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&request).to_string());
            let body = "event: response.completed\ndata: {\"type\":\"response.completed\",\
                        \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
                        \"usage\":{\"input_tokens\":1000,\"input_tokens_details\":\
                        {\"cached_tokens\":900},\"output_tokens\":16}}}\n\n";
            sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
            sock.flush().unwrap();
        }
        requests
    });

    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-6.1-sol", "sys", None)
        .with_prompt_cache(PromptCachePolicy::Implicit);
    let turns = vec![crate::project::WireTurn::text("hello")];
    let tools = vec![serde_json::json!({"type": "function", "name": "x"})];
    client
        .complete(&turns, &tools, ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let primer = client
        .prime(&turns, &tools, ToolChoice::force("retain"))
        .await
        .expect("an implicit Responses lineage takes a primer")
        .expect("the mock backend answers the primer");
    assert_eq!(primer.usage.cache_read_input_tokens, 900);
    assert_eq!(primer.usage.output_tokens, 16);

    let requests = server.join().unwrap();
    let body_of = |request: &str| {
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        serde_json::from_str::<serde_json::Value>(body).unwrap()
    };
    let mut turn_body = body_of(&requests[0]);
    let mut primer_body = body_of(&requests[1]);
    assert!(
        !turn_body.to_string().contains("max_output_tokens"),
        "a turn sends no cap"
    );
    assert_eq!(
        primer_body.get("max_output_tokens"),
        Some(&serde_json::json!(16)),
        "the primer caps its output at the backend minimum"
    );
    assert_eq!(
        primer_body.get("tool_choice"),
        Some(&serde_json::json!({"type": "function", "name": "retain"})),
        "the primer forces retain, matching the peek's head rather than the turn's auto"
    );
    assert_eq!(
        primer_body.get("parallel_tool_calls"),
        Some(&serde_json::json!(false)),
        "forcing retain disables parallel calls, as the peek sends it"
    );
    for field in ["max_output_tokens", "tool_choice", "parallel_tool_calls"] {
        primer_body.as_object_mut().unwrap().remove(field);
        turn_body.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(
        primer_body, turn_body,
        "the primer replays the turn's input byte-for-byte apart from cap and choice"
    );
}

/// The primer plants only where an implicit cache exists behind a Responses lineage:
/// Messages dialects and explicit policies resolve to no primer without sending.
#[tokio::test]
async fn primer_sends_nothing_without_an_implicit_responses_lineage() {
    let messages = Client::new(
        Endpoint {
            provider: Provider::Anthropic,
            url: None,
            auth_header: "x".into(),
            auth_value: "y".into(),
            session_id: None,
            extra_headers: Vec::new(),
            request: Default::default(),
            base: None,
        },
        "claude-haiku-4-5",
        "sys",
        None,
    );
    assert!(
        messages.prime(&[], &[], ToolChoice::auto()).await.is_none(),
        "no primer on a Messages lineage"
    );

    let explicit = Client::new(
        Endpoint {
            provider: Provider::OpenAICodex,
            url: None,
            auth_header: "x".into(),
            auth_value: "y".into(),
            session_id: None,
            extra_headers: Vec::new(),
            request: Default::default(),
            base: None,
        },
        "gpt-6.1-sol",
        "sys",
        None,
    )
    .with_prompt_cache(PromptCachePolicy::Explicit);
    assert!(
        explicit.prime(&[], &[], ToolChoice::auto()).await.is_none(),
        "no primer where the harness places explicit breakpoints"
    );
}

/// The primer wants the write, not the read: a 16-token budget at a call-eliciting
/// position truncates the model's function call mid-arguments, which marks the
/// completion without unmaking its input prefix. The primer still plants.
/// (backlog/separate-triage-cold-prefix.md — pinned after the first live run
/// showed every primer failing this way.)
#[tokio::test]
async fn primer_plants_despite_truncated_output() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = vec![0u8; 65536];
        loop {
            let n = sock.read(&mut buf).unwrap();
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if n == 0 || body.len() >= length {
                break;
            }
        }
        let body = "event: response.completed\ndata: {\"type\":\"response.completed\",\
                    \"response\":{\"id\":\"r\",\"status\":\"completed\",\
                    \"output\":[{\"type\":\"function_call\",\"call_id\":\"c\",\
                    \"name\":\"bash\",\"arguments\":\"{not json\"}],\
                    \"usage\":{\"input_tokens\":1000,\"output_tokens\":16}}}\n\n";
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        sock.flush().unwrap();
    });

    let endpoint = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some(format!("http://{addr}/backend-api/codex/responses")),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "gpt-6-luna", "sys", None)
        .with_prompt_cache(PromptCachePolicy::Implicit);
    let turns = vec![crate::project::WireTurn::text("hello")];
    let primer = client
        .prime(&turns, &[], ToolChoice::auto())
        .await
        .expect("an implicit Responses lineage takes a primer")
        .expect("truncated output still plants where input was processed");
    assert_eq!(primer.usage.input_tokens, 1000);
    server.join().unwrap();
}
