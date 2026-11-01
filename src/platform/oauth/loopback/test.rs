use super::*;

use trillium_client::Client;
use trillium_tokio::ClientConfig;

/// A ladder well away from the real one (50505–50507), wide enough that a
/// busy machine does not make these tests flaky.
const TEST_PORTS: [u16; 8] = [50911, 50912, 50913, 50914, 50915, 50916, 50917, 50918];

fn client() -> Client {
    Client::new(ClientConfig::default())
}

#[test]
fn a_redirect_uri_names_the_callback_path() {
    assert_eq!(
        redirect_uri("127.0.0.1", 50505),
        "http://127.0.0.1:50505/callback"
    );
    assert_eq!(
        redirect_uri("localhost", 50507),
        "http://localhost:50507/callback"
    );
}

/// Authorization servers match redirect URIs exactly and differ on which host
/// spelling they accept, so every rung is declared in both spellings.
#[test]
fn every_rung_is_declared_in_both_spellings() {
    assert_eq!(
        all_redirect_uris(&[50505, 50506]),
        [
            "http://127.0.0.1:50505/callback",
            "http://localhost:50505/callback",
            "http://127.0.0.1:50506/callback",
            "http://localhost:50506/callback",
        ]
    );
}

#[test]
fn a_query_decodes_into_pairs() {
    assert_eq!(
        decode_query("code=abc&state=xyz"),
        [("code", "abc"), ("state", "xyz")].map(|(k, v)| (k.to_string(), v.to_string()))
    );
    // Percent-encoded values are decoded, so an `iss` comparison sees the
    // real issuer rather than an escaped one.
    assert_eq!(
        decode_query("iss=https%3A%2F%2Fauth.example.com"),
        [("iss".to_string(), "https://auth.example.com".to_string())]
    );
    assert!(decode_query("").is_empty());
}

/// The end-to-end shape: bind, have a browser stand-in navigate to the
/// callback, and get the decoded query back.
#[tokio::test]
async fn a_callback_delivers_its_query() {
    let loopback = listen(&TEST_PORTS).unwrap();
    let uri = loopback.redirect_uri();
    assert!(TEST_PORTS.contains(&loopback.port()));

    let request = tokio::spawn(async move {
        client()
            .get(
                format!("{uri}?code=the-code&state=the-state&iss=https%3A%2F%2Fauth.example.com")
                    .as_str(),
            )
            .await
            .unwrap()
            .status()
            .unwrap()
    });

    let params = loopback.wait().await.unwrap();
    assert_eq!(
        params,
        [
            ("code", "the-code"),
            ("state", "the-state"),
            ("iss", "https://auth.example.com"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    assert_eq!(u16::from(request.await.unwrap()), 200);
}

/// A response with no query at all still resolves the wait — the validation
/// that rejects it belongs to `authorize::Pending`, not here.
#[tokio::test]
async fn an_empty_callback_still_resolves() {
    let loopback = listen(&TEST_PORTS).unwrap();
    let uri = loopback.redirect_uri();
    tokio::spawn(async move { client().get(uri.as_str()).await });
    assert!(loopback.wait().await.unwrap().is_empty());
}

/// Anything that is not the callback is not an authorization response, and
/// must not resolve the wait.
#[tokio::test]
async fn other_paths_do_not_deliver() {
    let loopback = listen(&TEST_PORTS).unwrap();
    let port = loopback.port();
    let stray = client()
        .get(format!("http://127.0.0.1:{port}/favicon.ico").as_str())
        .await
        .unwrap();
    assert_eq!(u16::from(stray.status().unwrap()), 404);

    // The real callback still works afterwards.
    let uri = loopback.redirect_uri();
    tokio::spawn(async move { client().get(format!("{uri}?code=c").as_str()).await });
    assert_eq!(
        loopback.wait().await.unwrap(),
        [("code".to_string(), "c".to_string())]
    );
}

/// A refresh or a stale tab must not overwrite a response already in hand.
#[tokio::test]
async fn a_second_callback_does_not_replace_the_first() {
    let loopback = listen(&TEST_PORTS).unwrap();
    let uri = loopback.redirect_uri();

    let first = client()
        .get(format!("{uri}?code=first").as_str())
        .await
        .unwrap();
    assert_eq!(u16::from(first.status().unwrap()), 200);

    let mut second = client()
        .get(format!("{uri}?code=second").as_str())
        .await
        .unwrap();
    assert_eq!(u16::from(second.status().unwrap()), 200);
    let body = second.response_body().read_string().await.unwrap();
    assert!(body.contains("Already received"), "{body}");

    assert_eq!(
        loopback.wait().await.unwrap(),
        [("code".to_string(), "first".to_string())]
    );
}

/// After the response is taken, the port must not stay open — a live loopback
/// listener is a standing invitation to feed this process an unsolicited code.
#[tokio::test]
async fn the_port_closes_once_the_response_is_taken() {
    let loopback = listen(&TEST_PORTS).unwrap();
    let port = loopback.port();
    let uri = loopback.redirect_uri();
    tokio::spawn(async move { client().get(format!("{uri}?code=c").as_str()).await });
    loopback.wait().await.unwrap();

    // Rebinding the same port is the observable proof the listener is gone.
    let rebound = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(again) = listen(&[port]) {
                return again;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(rebound.is_ok(), "port {port} was still bound after wait()");
}
