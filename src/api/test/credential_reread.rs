//! Tests for the one re-read of a credential's source on any rejected credential
//! (docs/transport.md), over a loopback server that answers each request from a
//! script and records the credential each carried.

use super::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A stream that finalizes with one line of text.
const ANSWER: &str = "event: message_start\n\
    data: {\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n\
    event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
    event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n\
    event: content_block_stop\ndata: {\"index\":0}\n\n\
    event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
    event: message_stop\ndata: {}\n\n";

const REVOKED: &str = "{\"error\":{\"type\":\"authentication_error\",\"code\":\"invalid_api_key\",\"message\":\"revoked\"}}";
const UNRECOGNIZED: &str = "{\"error\":{\"code\":\"grant_unknown\",\"message\":\"no such grant\"}}";
const NOT_JSON: &str = "<html>forbidden by the gateway</html>";
const QUOTA: &str = "{\"error\":{\"type\":\"insufficient_quota\",\"message\":\"out of credit\"}}";

/// One canned answer: a status line and a body, JSON unless it is the event stream.
struct Reply(&'static str, &'static str);

/// What one scripted call did: its result, the `x-api-key` each request carried,
/// and how many times the credential's source was read.
struct Run {
    result: Result<(), String>,
    sent: Vec<String>,
    reads: usize,
}

/// A client whose cell holds `stale` and whose source yields `source`, against a
/// server that answers each request from `script` and then stops listening.
async fn run(script: Vec<Reply>, source: &'static str) -> Run {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&sent);
    let server = std::thread::spawn(move || {
        for Reply(status, body) in script {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let key = request
                .lines()
                .find_map(|line| line.strip_prefix("x-api-key: "))
                .unwrap_or_default()
                .to_string();
            seen.lock().unwrap().push(key);
            let content_type = if status.starts_with("200") {
                "text/event-stream"
            } else {
                "application/json"
            };
            sock.write_all(
                format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        }
    });
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: crate::api::AuthValue::reloadable(
            "stale".into(),
            crate::api::sync_reload(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(source.into())
            }),
        ),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Client::new(endpoint, "m", "sys", None);
    let result = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    // Unblocks a server still waiting on a request that never came.
    let _ = std::net::TcpStream::connect(addr);
    server.join().unwrap();
    let sent = sent.lock().unwrap().clone();
    Run {
        result,
        sent,
        reads: reads.load(Ordering::SeqCst),
    }
}

/// A key the operator revoked and re-authorized: the cell holds the dead value, the
/// source holds the new one, and the second request carries it.
#[tokio::test]
async fn a_revoked_key_is_re_read_and_the_attempt_retried() {
    let run = run(
        vec![Reply("401 Unauthorized", REVOKED), Reply("200 OK", ANSWER)],
        "fresh",
    )
    .await;
    assert_eq!(run.result, Ok(()));
    assert_eq!(run.sent, ["stale", "fresh"]);
    assert_eq!(run.reads, 1);
}

/// A source still holding the dead value costs one read and one request: the stall
/// carries the provider's words and says what was tried, and nothing loops.
#[tokio::test]
async fn a_revoked_key_the_source_still_holds_fails_once() {
    let run = run(vec![Reply("401 Unauthorized", REVOKED)], "stale").await;
    let error = run.result.unwrap_err();
    assert!(error.contains("invalid_api_key"), "{error}");
    assert!(error.contains("credential re-read"), "{error}");
    assert_eq!(run.sent, ["stale"]);
    assert_eq!(run.reads, 1);
}

/// A 401 naming nothing the table knows is read the same way, whatever its body.
#[tokio::test]
async fn an_unrecognized_401_is_re_read_once() {
    for body in [UNRECOGNIZED, NOT_JSON] {
        let replaced = run(
            vec![Reply("401 Unauthorized", body), Reply("200 OK", ANSWER)],
            "fresh",
        )
        .await;
        assert_eq!(replaced.result, Ok(()), "{body}");
        assert_eq!(replaced.sent, ["stale", "fresh"], "{body}");

        let unchanged = run(vec![Reply("401 Unauthorized", body)], "stale").await;
        assert!(unchanged.result.is_err(), "{body}");
        assert_eq!(unchanged.sent, ["stale"], "{body}");
        assert_eq!(unchanged.reads, 1, "{body}");
    }
}

/// A 403 is a rejected credential as a 401 is.
#[tokio::test]
async fn a_403_is_re_read_once() {
    let run = run(
        vec![Reply("403 Forbidden", NOT_JSON), Reply("200 OK", ANSWER)],
        "fresh",
    )
    .await;
    assert_eq!(run.result, Ok(()));
    assert_eq!(run.sent, ["stale", "fresh"]);
}

/// A refusal that names the account rather than the credential (a quota, a 429) does
/// not read the source: a new value could not change it.
#[tokio::test]
async fn a_refusal_that_is_not_the_credentials_does_not_read_the_source() {
    for reply in [
        Reply("403 Forbidden", QUOTA),
        Reply("401 Unauthorized", QUOTA),
    ] {
        let run = run(vec![reply], "fresh").await;
        assert!(run.result.is_err());
        assert_eq!(run.sent, ["stale"]);
        assert_eq!(run.reads, 0);
    }
}

#[test]
fn the_credential_markers_are_the_terminal_names_about_the_credential() {
    use crate::api::failure::rejects_credential;

    for body in [
        "{\"error\":{\"code\":\"token_expired\"}}",
        "{\"error\":{\"code\":\"invalid_api_key\"}}",
        "{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}",
        "{\"error\":{\"type\":\"permission_error\"}}",
        "{\"error\":{\"code\":\"something_new\"}}",
        "",
        "not json",
    ] {
        assert!(rejects_credential(body), "{body}");
    }
    for body in [
        "{\"error\":{\"type\":\"insufficient_quota\"}}",
        "{\"error\":{\"code\":\"usage_limit_reached\"}}",
        "{\"error\":{\"type\":\"billing_error\"}}",
        "{\"error\":{\"code\":\"model_not_found\"}}",
    ] {
        assert!(!rejects_credential(body), "{body}");
    }
}
