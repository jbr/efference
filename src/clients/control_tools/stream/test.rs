//! The stream server's routes (docs/operating.md): one loopback URL per agent, a 404 for
//! any path that names no stream, and a refusal for a request to a stream that is not an
//! upgrade. What a stream sends is tested over a host (`host/test/host/control/`).

use super::*;
use trillium_client::websocket::ErrorKind;
use trillium_tokio::ClientConfig;

/// A host client that answers nothing: these tests never follow a session.
fn no_host() -> Client {
    Client::with_connector(trillium_testing::ServerConnector::new(
        |conn: Conn| async move { conn.with_status(Status::NotFound).halt() },
    ))
}

fn http() -> trillium_client::Client {
    trillium_client::Client::new(ClientConfig::default())
}

/// `agent`'s stream URL on `streams`, spelled for an HTTP client.
async fn http_url(streams: &Streams, agent: &str) -> String {
    let url = streams.url(&no_host(), agent, None).await.unwrap();
    url.replacen("ws://", "http://", 1)
}

/// An HTTP GET of `url`'s status.
async fn status_of(url: &str) -> u16 {
    let conn = http().get(url).await.unwrap();
    u16::from(conn.status().unwrap())
}

/// One server on loopback; one token per agent, the same on every request for it, and
/// long and URL-safe.
#[tokio::test]
async fn each_agent_has_one_loopback_url() {
    let streams = Streams::default();
    let host = no_host();
    let child = streams.url(&host, "child", None).await.unwrap();
    assert_eq!(streams.url(&host, "child", None).await.unwrap(), child);
    let other = streams.url(&host, "other", None).await.unwrap();
    assert_ne!(other, child);

    let (base, token) = child.rsplit_once('/').unwrap();
    assert!(base.starts_with("ws://127.0.0.1:"), "{child}");
    assert_eq!(other.rsplit_once('/').unwrap().0, base, "one server");
    assert_eq!(token.len(), 43, "{token}");
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "{token}"
    );
}

/// A path that is not exactly a stream's token is a 404, upgrade or not.
#[tokio::test]
async fn a_path_that_names_no_stream_is_a_404() {
    let streams = Streams::default();
    let url = http_url(&streams, "child").await;
    let (base, token) = url.rsplit_once('/').unwrap();
    for path in [
        "/".to_string(),
        "/wrong".to_string(),
        format!("/{token}/more"),
        format!("/{token}x"),
    ] {
        assert_eq!(status_of(&format!("{base}{path}")).await, 404, "{path}");
    }
    let refused = http()
        .get(format!("{base}/wrong").as_str())
        .into_websocket()
        .await
        .unwrap_err();
    assert!(
        matches!(refused.kind, ErrorKind::Status(Status::NotFound)),
        "{:?}",
        refused.kind
    );
}

/// A stream's path answers only an upgrade: a plain request is a 426.
#[tokio::test]
async fn a_stream_path_without_an_upgrade_is_refused() {
    let streams = Streams::default();
    let url = http_url(&streams, "child").await;
    assert_eq!(status_of(&url).await, 426);
}
