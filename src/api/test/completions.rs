//! A described Completions connection end to end: `Client::complete` posts to
//! `{base}/chat/completions` with a bearer credential, and folds a recorded vendor
//! stream into a Completions turn (docs/transport.md).

use std::io::{Read, Write};

use super::*;

/// Serve one recorded SSE capture and hand back the request it answered.
fn serve(capture: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        // Read until the body named by `content-length` is in.
        let mut request = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            let read = sock.read(&mut buf).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
            let text = String::from_utf8_lossy(&request);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\
             content-length: {}\r\n\r\n{capture}",
            capture.len()
        );
        sock.write_all(response.as_bytes()).unwrap();
        let _ = sock.flush();
        String::from_utf8_lossy(&request).into_owned()
    });
    (base, server)
}

#[tokio::test]
async fn a_described_completions_connection_round_trips() {
    let (base, server) = serve(include_str!("../completions/mistral_tool.sse"));
    let endpoint = Endpoint::from_credentials(
        Provider::Custom(WireDialect::Completions),
        "KEY".into(),
        None,
        Some(base),
    )
    .unwrap();
    let client = Client::new(endpoint, "mistral-small-latest", "PRE", None);
    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run",
        "input_schema": {"type": "object"},
    })];
    let completion = client
        .complete(
            &[crate::project::WireTurn::text("list")],
            &tools,
            ToolChoice::auto(),
            |_| {},
        )
        .await
        .unwrap();
    let request = server.join().unwrap();

    let (head, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("POST /v1/chat/completions "), "{head}");
    assert!(
        head.lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer KEY")),
        "{head}"
    );
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["model"], "mistral-small-latest");
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role": "system", "content": "PRE"})
    );
    assert_eq!(
        body["messages"][1],
        serde_json::json!({"role": "user", "content": "list"})
    );
    assert_eq!(body["tools"][0]["function"]["name"], "bash");
    assert_eq!(body["stream_options"]["include_usage"], true);

    let calls = completion.assistant_output().tool_calls().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::ToolCalls)
    );
    assert!(completion.upstream_failure().is_none());
}
