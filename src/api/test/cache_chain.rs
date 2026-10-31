//! The cache chain end to end (docs/transport.md): each turn names the response before
//! it, so the provider can compare two prompts and say what moved — and a provider that
//! does not read the field is sent nothing.

use std::io::{Read, Write};
use std::sync::mpsc;

use super::*;

/// One finalized text turn that named `id` for itself.
fn stream(id: &str) -> String {
    format!(
        "event: message_start\ndata: {{\"message\":{{\"id\":\"{id}\",\"model\":\"m\",\
         \"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n\
         event: content_block_start\ndata: {{\"index\":0,\"content_block\":\
         {{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
         event: content_block_delta\ndata: {{\"index\":0,\"delta\":\
         {{\"type\":\"text_delta\",\"text\":\"ok\"}}}}\n\n\
         event: content_block_stop\ndata: {{\"index\":0}}\n\n\
         event: message_stop\ndata: {{}}\n\n"
    )
}

/// Serve one response per entry of `responses`, in order, on one listener, handing back
/// the base URL and each request as it arrives. Sequential because the chain is: what the
/// second request names depends on what the first response said.
fn serve(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for capture in responses {
            let (mut sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Read until the body `content-length` named is in.
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
            tx.send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
        }
    });
    (base, rx)
}

/// An endpoint that points `provider` at a local server.
fn endpoint(base: &str, provider: Provider) -> Endpoint {
    Endpoint::at(provider, format!("{base}/messages"))
}

/// Two turns on a native Anthropic client: the first opts in with an explicit `null`, the
/// second names the first response's id. This is the whole mechanism — the object is the
/// opt-in that stores the fingerprint the next request is compared against
/// (docs/transport.md) — and it is also what keeps the chain off every other provider.
#[tokio::test]
async fn consecutive_turns_name_each_others_responses() {
    let (base, requests) = serve(vec![stream("msg_a"), stream("msg_b")]);
    let client = Client::new(endpoint(&base, Provider::Anthropic), "m", "SYS", None)
        .with_origin(crate::log::RequestOrigin::Turn);

    for _ in 0..2 {
        client
            .complete(&[], &[], ToolChoice::auto(), |_| {})
            .await
            .expect("the mock stream finalizes");
    }

    let opening = requests.recv().unwrap();
    assert!(
        opening.contains(r#""diagnostics":{"previous_message_id":null}"#),
        "the first request opts in with nothing to compare: {opening}"
    );
    let second = requests.recv().unwrap();
    assert!(
        second.contains(r#""diagnostics":{"previous_message_id":"msg_a"}"#),
        "the second names the first response: {second}"
    );
}

/// The same two turns on a provider that does not read the field: the object is absent
/// from both bodies, byte for byte as if the feature did not exist. OpenRouter's Messages
/// skin accepts the object and answers a constant `null`, which a session cannot tell
/// apart from "no divergence" (docs/transport.md), so it is left off.
#[tokio::test]
async fn a_provider_without_the_field_is_sent_no_object() {
    let (base, requests) = serve(vec![stream("msg_a"), stream("msg_b")]);
    let client = Client::new(endpoint(&base, Provider::OpenRouter), "m", "SYS", None)
        .with_origin(crate::log::RequestOrigin::Turn);

    for _ in 0..2 {
        client
            .complete(&[], &[], ToolChoice::auto(), |_| {})
            .await
            .expect("the mock stream finalizes");
    }

    for _ in 0..2 {
        let body = requests.recv().unwrap();
        assert!(!body.contains("diagnostics"), "{body}");
    }
}

/// A triage rides the same client and the same prefix but sends its own prompt, so it must
/// not add itself to the chain: naming its response as the next turn's comparison base
/// would have the provider report a difference in `messages` that the loop never made
/// (docs/transport.md).
#[tokio::test]
async fn a_triage_neither_opts_in_nor_extends_the_chain() {
    let (base, requests) = serve(vec![
        stream("msg_turn"),
        stream("msg_triage"),
        stream("msg_a"),
    ]);
    let endpoint = endpoint(&base, Provider::Anthropic);
    let client = Client::new(endpoint.clone(), "m", "SYS", None)
        .with_origin(crate::log::RequestOrigin::Turn);

    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the mock stream finalizes");
    // The triage, on the agent's own client, as the triager builds it.
    client
        .with_origin(crate::log::RequestOrigin::Triage)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the mock stream finalizes");
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the mock stream finalizes");

    let _turn = requests.recv().unwrap();
    let triage = requests.recv().unwrap();
    assert!(
        !triage.contains("diagnostics"),
        "a triage sends its own prompt, so it neither opts in nor is compared: {triage}"
    );
    let next = requests.recv().unwrap();
    assert!(
        next.contains(r#""diagnostics":{"previous_message_id":"msg_turn"}"#),
        "the turn after the triage still names the last *turn*: {next}"
    );
}
