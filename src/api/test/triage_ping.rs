//! On a lineage the primer plants, the triage keepalive's ping is the primer's own request
//! (docs/transport.md): over the same turns, envelope, and choice, the two sends carry the
//! same body bytes. The ping differs only in its request origin, which rides the stage
//! record and the usage line, not the body.

use super::*;
use crate::log::RequestOrigin;

/// One mock Responses backend that answers two requests, each on a fresh connection,
/// and returns the raw request text it received, in order.
fn two_request_backend() -> (std::net::SocketAddr, std::thread::JoinHandle<Vec<String>>) {
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
    (addr, server)
}

/// The primer and the triage keepalive, sent over the same turns with the triage's
/// envelope, carry byte-identical bodies. A ping that re-sent anything else would
/// refresh a different entry from the one the primer planted.
#[tokio::test]
async fn the_triage_ping_body_is_the_primers_body() {
    let (addr, server) = two_request_backend();
    let endpoint = Endpoint::at(
        Provider::OpenAICodex,
        format!("http://{addr}/backend-api/codex/responses"),
    );
    let client = Client::new(endpoint, "gpt-6.1-sol", "sys", None)
        .with_prompt_cache(PromptCachePolicy::Implicit);
    let turns = vec![crate::project::WireTurn::text("hello")];
    let tools = vec![serde_json::json!({"type": "function", "name": "retain"})];
    let choice = ToolChoice::force("retain");

    let primer = client
        .prime(&turns, &tools, choice)
        .await
        .expect("an implicit Responses lineage takes a primer")
        .expect("the backend answers the primer");
    let ping = client
        .keepalive(
            RequestOrigin::TriageKeepalive,
            &SentRequest::new(turns.clone(), tools.clone(), choice),
        )
        .await
        .expect("the backend answers the ping");
    assert_eq!(primer.usage.cache_read_input_tokens, 900);
    assert_eq!(ping.usage.cache_read_input_tokens, 900);

    let requests = server.join().unwrap();
    let body_of = |request: &str| request.split_once("\r\n\r\n").unwrap().1.to_string();
    assert_eq!(
        body_of(&requests[1]),
        body_of(&requests[0]),
        "the ping sends the primer's body"
    );
    assert!(
        body_of(&requests[1]).contains(r#""max_output_tokens":16"#),
        "and the ping keeps the primer's output cap"
    );
}
