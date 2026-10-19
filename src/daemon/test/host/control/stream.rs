//! The MCP mount's status streams over a host, as Claude Code's `Monitor` reads them
//! (docs/operating.md): the status on connect, each new decision point, the end and its
//! close, every connection its own sequence, and an error close when the host stops
//! answering.

use super::*;
use crate::control_tools::{McpStatus, Wait};
use crate::log::CallId;
use futures_lite::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use trillium_client::WebSocketConn;
use trillium_client::websocket::Message;
use trillium_websockets::tungstenite::protocol::frame::coding::CloseCode;

/// `status(agent, follow: true)` over the mount: its answer, and the URL its follow line
/// gives.
async fn followed(control: &Control, agent: &str) -> (String, String) {
    let said = McpStatus {
        agent: Some(agent.into()),
        follow: true,
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap();
    let url = said
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("follow: "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no follow line: {said}"))
        .to_string();
    (said, url)
}

/// A WebSocket client on the stream at `url`.
async fn open(url: &str) -> WebSocketConn {
    trillium_client::Client::new(trillium_tokio::ClientConfig::default())
        .get(url.replacen("ws://", "http://", 1).as_str())
        .into_websocket()
        .await
        .expect("the stream opens")
}

/// What the stream sends next, or a panic after ten seconds of nothing.
async fn next(ws: &mut WebSocketConn) -> Message {
    match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        Ok(Some(Ok(message))) => message,
        other => panic!("expected a message, got {other:?}"),
    }
}

/// The next frame's text.
async fn frame(ws: &mut WebSocketConn) -> String {
    match next(ws).await {
        Message::Text(text) => text.to_string(),
        other => panic!("expected a frame, got {other:?}"),
    }
}

/// The close the stream ends with: its code and reason.
async fn close(ws: &mut WebSocketConn) -> (CloseCode, String) {
    match next(ws).await {
        Message::Close(Some(close)) => (close.code, close.reason.to_string()),
        other => panic!("expected a close, got {other:?}"),
    }
}

/// The first line of `text`.
fn first(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// A stream sends the status at connect, then each new decision point and nothing for
/// working, then the end, and closes with 1000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_sends_the_status_then_each_decision_point_then_the_end() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, _) = mount(&host).await;
    let (said, url) = followed(&control, "child").await;
    assert_eq!(first(&said), "child: idle, its turn is done", "{said}");
    assert!(url.starts_with("ws://127.0.0.1:"), "{url}");
    assert!(
        said.ends_with(&format!(
            "\nfollow: {url} — in Claude Code, open a Monitor on it (ws) to be woken by each \
             decision point"
        )),
        "{said}"
    );

    let mut ws = open(&url).await;
    assert_eq!(
        first(&frame(&mut ws).await),
        "child: idle, its turn is done"
    );

    let (seq, _first) = hold(&host, "child", &bash("toolu_1", "ls ~"));
    let gate = frame(&mut ws).await;
    assert_eq!(
        first(&gate),
        format!("child: waiting for your decision on bash `ls ~` (seq {seq})"),
        "{gate}"
    );
    assert!(
        gate.contains(&format!("decide: decide(agent: \"child\", seq: \"{seq}\"")),
        "{gate}"
    );

    let session = host.session("child").unwrap();
    assert!(session.resolve_approval(&CallId("toolu_1".into()), Approval::Approve, None));
    let (_, _second) = hold(&host, "child", &bash("toolu_2", "pwd"));
    let gate = frame(&mut ws).await;
    assert!(
        first(&gate).starts_with("child: waiting for your decision on bash `pwd`"),
        "working sends no frame: {gate}"
    );

    Archive {
        agent: "child".into(),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap();
    assert_eq!(
        first(&frame(&mut ws).await),
        "child: ended; its log stays on disk"
    );
    assert_eq!(close(&mut ws).await.0, CloseCode::Normal);
    host.swansong().shut_down().await;
}

/// The same agent's URL is the same on every request; each connection to it gets the
/// whole sequence; and the stream and `wait` keep their own records, so neither takes a
/// status from the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_connection_and_wait_get_every_status() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, _) = mount(&host).await;
    let (_, url) = followed(&control, "child").await;
    assert_eq!(followed(&control, "child").await.1, url);

    let mut one = open(&url).await;
    let mut two = open(&url).await;
    for ws in [&mut one, &mut two] {
        assert_eq!(first(&frame(ws).await), "child: idle, its turn is done");
    }
    let waited = |timeout_s| Wait {
        agent: Some("child".into()),
        timeout_s: Some(timeout_s),
    };
    let said = waited(10)
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(first(&said), "child: idle, its turn is done");

    let (_, _decision) = hold(&host, "child", &bash("toolu_1", "ls ~"));
    for ws in [&mut one, &mut two] {
        assert!(
            first(&frame(ws).await).starts_with("child: waiting for your decision on bash `ls ~`")
        );
    }
    let said = waited(10)
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap();
    assert!(
        first(&said).starts_with("child: waiting for your decision on bash `ls ~`"),
        "{said}"
    );

    Archive {
        agent: "child".into(),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap();
    for ws in [&mut one, &mut two] {
        assert_eq!(
            first(&frame(ws).await),
            "child: ended; its log stays on disk"
        );
        assert_eq!(close(ws).await.0, CloseCode::Normal);
    }
    host.swansong().shut_down().await;
}

/// A host that stops answering closes the stream with 1011 and says why, so a closed
/// stream never reads as an agent still working.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_closes_with_an_error_when_the_host_stops_answering() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let down = Arc::new(AtomicBool::new(false));
    let front = {
        let down = Arc::clone(&down);
        move |conn: trillium::Conn| {
            let down = Arc::clone(&down);
            async move {
                match down.load(Ordering::SeqCst) {
                    true => conn.with_status(503).halt(),
                    false => conn,
                }
            }
        }
    };
    let (client, server) = host_root_behind(&host, front).await;
    let (control, _) = mount_over(client);
    let (_, url) = followed(&control, "child").await;
    let mut ws = open(&url).await;
    assert_eq!(
        first(&frame(&mut ws).await),
        "child: idle, its turn is done"
    );

    // The server stops: its event streams end, and nothing it is asked answers.
    down.store(true, Ordering::SeqCst);
    server.shut_down().await;
    let (code, reason) = close(&mut ws).await;
    assert_eq!(code, CloseCode::Error);
    assert!(reason.contains("unreachable"), "{reason}");
    host.swansong().shut_down().await;
}

/// A stream is one agent's: `follow` on the host-wide overview names the argument that
/// would work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_names_one_agent() {
    let roots = Roots::new();
    let host = hosting(&roots, &["child"]).await;
    let (control, _) = mount(&host).await;
    let err = McpStatus {
        agent: None,
        follow: true,
    }
    .execute(control, RequestContext::default())
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "`follow` streams one agent: name it with `agent`"
    );
    host.swansong().shut_down().await;
}
