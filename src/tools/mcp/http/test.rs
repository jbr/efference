//! Renewing an MCP server's stored credentials mid-session (docs/mcp-auth.md),
//! against a fake MCP server and the token endpoint of a fake authorization
//! server on one loopback listener. Credentials come from an in-memory secret
//! store seeded with the record the real store holds.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mcplease::types::{
    CallToolResult, JsonRpcError, JsonRpcMessage, JsonRpcResponse, Tool as ToolDefinition,
};
use serde_json::{Value, json};
use trillium::Conn as ServerConn;

use crate::config::McpTransport;
use crate::platform::secrets::Secrets;
use crate::record::log::{RawResult, ToolCall};
use crate::tools::bash::Outcome;
use crate::tools::mcp::auth::store::{KeychainStore, TokenStore};
use crate::tools::mcp::auth::{Credentials, stored_credentials};
use crate::tools::mcp::{ConnectedProvider, Session, Transport};

use super::HttpTransport;

const SERVER: &str = "guarded";

/// The two parties a renewal talks to. The MCP server accepts one access token
/// and answers anything else `401`; the token endpoint trades the one refresh
/// token it honours for a new grant, which the MCP server then accepts, and
/// rotates it, refusing the old one if it is presented again as an OAuth 2.1
/// server refuses a public client's replay.
#[derive(Default)]
struct Parties {
    /// The access token the MCP server accepts; `None` accepts nothing.
    accepted: Mutex<Option<String>>,
    /// Answer `401` even to the accepted token.
    refuse_everything: AtomicBool,
    /// The refresh token the token endpoint honours; `None` honours none.
    honoured: Mutex<Option<String>>,
    /// How long the token endpoint takes to answer a refresh.
    delay: Mutex<Duration>,
    /// Refresh requests the token endpoint received, honoured or not.
    refreshes: AtomicUsize,
    /// Each JSON-RPC request the MCP server received: its method and the
    /// `Authorization` header it carried.
    requests: Mutex<Vec<(String, Option<String>)>>,
}

impl Parties {
    fn refreshes(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }

    /// The `Authorization` header on each `tools/call`, refused or answered.
    fn calls(&self) -> Vec<Option<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "tools/call")
            .map(|(_, authorization)| authorization.clone())
            .collect()
    }

    /// The access token is revoked at the MCP server; its refresh token still works.
    fn revoke_access(&self) {
        *self.accepted.lock().unwrap() = None;
    }

    async fn handle(&self, mut conn: ServerConn) -> ServerConn {
        let body = conn.request_body_string().await.unwrap_or_default();
        match conn.path() {
            "/token" => self.token(conn, &body).await,
            _ => self.mcp(conn, &body),
        }
    }

    async fn token(&self, conn: ServerConn, body: &str) -> ServerConn {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        let form: Vec<(String, String)> =
            trillium_client::url::form_urlencoded::parse(body.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
        let presented = form
            .iter()
            .find(|(k, _)| k == "refresh_token")
            .map(|(_, v)| v.clone());
        let mut honoured = self.honoured.lock().unwrap();
        if presented.is_none() || presented != *honoured {
            return conn
                .with_status(400)
                .with_response_header("content-type", "application/json")
                .with_body(r#"{"error":"invalid_grant","error_description":"refresh token is not current"}"#);
        }
        let n = self.refreshes();
        let (access, refresh) = (format!("at-{n}"), format!("rt-{n}"));
        *honoured = Some(refresh.clone());
        *self.accepted.lock().unwrap() = Some(access.clone());
        conn.with_status(200)
            .with_response_header("content-type", "application/json")
            .with_body(
                json!({
                    "access_token": access,
                    "token_type": "Bearer",
                    "refresh_token": refresh,
                    "expires_in": 3600,
                })
                .to_string(),
            )
    }

    fn mcp(&self, conn: ServerConn, body: &str) -> ServerConn {
        let authorization = conn
            .request_headers()
            .get_str("authorization")
            .map(String::from);
        let Ok(JsonRpcMessage::Request(request)) = serde_json::from_str(body) else {
            // A notification: accepted without an answer.
            return conn.with_status(202);
        };
        self.requests
            .lock()
            .unwrap()
            .push((request.method.clone(), authorization.clone()));
        let accepted = self.accepted.lock().unwrap().clone();
        if self.refuse_everything.load(Ordering::SeqCst)
            || accepted.is_none()
            || authorization != accepted.map(|token| format!("Bearer {token}"))
        {
            return conn
                .with_status(401)
                .with_response_header("www-authenticate", "Bearer");
        }
        let result: Value = match request.method.as_str() {
            "server/discover" => json!({
                "supportedVersions": ["2026-07-28"],
                "capabilities": { "tools": {} },
                "resultType": "complete",
            }),
            "tools/list" => json!({
                "tools": [ToolDefinition::new("echo", json!({ "type": "object" }))],
            }),
            "tools/call" => serde_json::to_value(CallToolResult::text("echoed")).unwrap(),
            other => {
                let response =
                    JsonRpcResponse::error(request.id, JsonRpcError::method_not_found(other));
                return answer(conn, &JsonRpcMessage::Response(response));
            }
        };
        answer(
            conn,
            &JsonRpcMessage::Response(JsonRpcResponse::success(request.id, result)),
        )
    }
}

fn answer(conn: ServerConn, message: &JsonRpcMessage) -> ServerConn {
    conn.with_status(200)
        .with_response_header("content-type", "application/json")
        .with_body(serde_json::to_string(message).unwrap())
}

/// A running pair of parties, and an in-memory store holding a grant for
/// [`SERVER`] issued by them: access token `at-0`, refresh token `rt-0`, expiring
/// at `expires_at` (unix seconds).
struct Fixture {
    url: String,
    parties: Arc<Parties>,
    store: Arc<dyn TokenStore>,
}

impl Fixture {
    async fn new(expires_at: u64) -> Self {
        let parties = Arc::new(Parties::default());
        *parties.accepted.lock().unwrap() = Some("at-0".into());
        *parties.honoured.lock().unwrap() = Some("rt-0".into());
        let handler_parties = Arc::clone(&parties);
        let handle = trillium_tokio::config()
            .without_signals()
            .listeners()
            .bind_tcp(("127.0.0.1", 0))
            .unwrap()
            .spawn(move |conn: ServerConn| {
                let parties = Arc::clone(&handler_parties);
                async move { parties.handle(conn).await }
            });
        let base = format!(
            "http://{}",
            handle
                .info()
                .await
                .tcp_socket_addr()
                .expect("a TCP listener")
        );
        let secrets = Arc::new(Secrets::in_memory());
        secrets
            .set(
                &crate::platform::secrets::mcp_name(SERVER),
                &json!({
                    "issuer": base,
                    "resource": format!("{base}/mcp"),
                    "token_endpoint": format!("{base}/token"),
                    "client_id": crate::tools::mcp::auth::CLIENT_ID_METADATA_DOCUMENT,
                    "tokens": {
                        "access_token": "at-0",
                        "token_type": "Bearer",
                        "refresh_token": "rt-0",
                        "expires_at": expires_at,
                    },
                })
                .to_string(),
            )
            .unwrap();
        Self {
            url: format!("{base}/mcp"),
            parties,
            store: Arc::new(KeychainStore::new(secrets)),
        }
    }

    /// Mount [`SERVER`] as a session's preparation does, with its stored credentials.
    async fn mount(&self) -> ConnectedProvider {
        ConnectedProvider::mount(
            SERVER,
            &McpTransport::Http {
                url: self.url.clone(),
            },
            &std::env::temp_dir(),
            stored_credentials(&self.store, SERVER).await.unwrap(),
            &crate::config::env_table::ChildEnv::default(),
        )
        .await
        .unwrap()
    }

    /// The access and refresh tokens the store holds for [`SERVER`].
    fn stored(&self) -> (String, Option<String>) {
        let stored = self.store.load(SERVER).unwrap().expect("a stored grant");
        (
            stored.tokens.access_token().to_string(),
            stored.tokens.refresh_token().map(String::from),
        )
    }
}

/// Far in the future: a grant that has not expired.
const LIVE: u64 = 4_000_000_000;
/// Long past: a grant that has.
const EXPIRED: u64 = 100;

async fn call(provider: &ConnectedProvider) -> RawResult {
    let call = ToolCall::new(format!("{SERVER}__echo"), "toolu_1", json!({}));
    match provider
        .run(&call, std::future::pending(), |_: &[u8]| {}, |_| {})
        .await
    {
        Outcome::Done(result) => result,
        Outcome::Detached(_) => panic!("a connected call never detaches"),
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// A call whose grant has expired is not sent with it: the transport refreshes
/// first, sends the call with the new grant, and stores what the refresh issued.
#[tokio::test]
async fn an_expired_grant_is_refreshed_before_the_call() {
    let fixture = Fixture::new(EXPIRED).await;
    let credentials = Credentials::load(&fixture.store, SERVER)
        .unwrap()
        .expect("a stored grant");
    assert!(credentials.is_expired());
    let mut session = Session::new(Transport::Http(
        HttpTransport::new(&fixture.url).with_credentials(Some(credentials)),
    ));
    let request = session
        .protocol
        .tools_call_with_progress("echo", &json!({}));

    let result = session.round_trip(request, |_| {}).await.unwrap().unwrap();

    assert_eq!(
        result,
        serde_json::to_value(CallToolResult::text("echoed")).unwrap()
    );
    assert_eq!(fixture.parties.refreshes(), 1);
    assert_eq!(
        fixture.parties.calls(),
        [Some(bearer("at-1"))],
        "the expired grant never reached the server"
    );
    assert_eq!(fixture.stored(), ("at-1".into(), Some("rt-1".into())));
}

/// A call the server refuses with `401` is refreshed and sent once more, and
/// succeeds; the refreshed grant is in the store afterwards.
#[tokio::test]
async fn a_refused_call_is_refreshed_and_retried_once() {
    let fixture = Fixture::new(LIVE).await;
    let provider = fixture.mount().await;
    fixture.parties.revoke_access();

    let result = call(&provider).await;

    assert!(!result.is_error, "{}", result.content);
    assert_eq!(result.content, "echoed");
    assert_eq!(fixture.parties.refreshes(), 1);
    assert_eq!(
        fixture.parties.calls(),
        [Some(bearer("at-0")), Some(bearer("at-1"))]
    );
    assert_eq!(fixture.stored(), ("at-1".into(), Some("rt-1".into())));
}

/// Two sessions' calls refused at once share one refresh: the second finds the
/// grant the first stored and sends it, rather than presenting the rotated
/// refresh token again. The token endpoint is slow enough that both renewals
/// are in flight together.
#[tokio::test]
async fn concurrent_calls_share_one_refresh() {
    let fixture = Fixture::new(LIVE).await;
    let (first, second) = (fixture.mount().await, fixture.mount().await);
    *fixture.parties.delay.lock().unwrap() = Duration::from_millis(200);
    fixture.parties.revoke_access();

    let (a, b) = tokio::join!(call(&first), call(&second));

    assert!(!a.is_error, "{}", a.content);
    assert!(!b.is_error, "{}", b.content);
    assert_eq!(fixture.parties.refreshes(), 1);
    let mut calls = fixture.parties.calls();
    calls.sort();
    assert_eq!(
        calls,
        [
            Some(bearer("at-0")),
            Some(bearer("at-0")),
            Some(bearer("at-1")),
            Some(bearer("at-1"))
        ]
    );
    assert_eq!(fixture.stored(), ("at-1".into(), Some("rt-1".into())));
}

/// A refresh the authorization server refuses fails the call, with a result
/// naming the server and saying a restart signs in again. Nothing is retried:
/// one refresh, one call.
#[tokio::test]
async fn a_failed_refresh_fails_the_call_without_looping() {
    let fixture = Fixture::new(LIVE).await;
    let provider = fixture.mount().await;
    fixture.parties.revoke_access();
    *fixture.parties.honoured.lock().unwrap() = None;

    let result = call(&provider).await;

    assert!(result.is_error);
    assert!(
        result
            .content
            .starts_with("`guarded` call failed: its sign-in expired"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("invalid_grant"),
        "{}",
        result.content
    );
    assert!(
        result.content.ends_with("a session restart signs in again"),
        "{}",
        result.content
    );
    assert_eq!(fixture.parties.refreshes(), 1);
    assert_eq!(fixture.parties.calls(), [Some(bearer("at-0"))]);
    assert_eq!(
        fixture.stored(),
        ("at-0".into(), Some("rt-0".into())),
        "a refused refresh leaves the stored grant as it was"
    );
}

/// A renewed grant the server refuses as well fails the call rather than
/// refreshing again: one refresh, two sends.
#[tokio::test]
async fn a_renewed_grant_refused_again_fails_the_call_without_looping() {
    let fixture = Fixture::new(LIVE).await;
    let provider = fixture.mount().await;
    fixture
        .parties
        .refuse_everything
        .store(true, Ordering::SeqCst);

    let result = call(&provider).await;

    assert!(result.is_error);
    assert!(
        result
            .content
            .contains("`guarded` call failed: its sign-in expired"),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("the server refused the renewed grant too"),
        "{}",
        result.content
    );
    assert!(
        result.content.ends_with("a session restart signs in again"),
        "{}",
        result.content
    );
    assert_eq!(fixture.parties.refreshes(), 1);
    assert_eq!(
        fixture.parties.calls(),
        [Some(bearer("at-0")), Some(bearer("at-1"))]
    );
}
