//! Tests for the TCP bind's credential state and the whole stack (docs/web-bind.md), over
//! the same in-memory transport the other daemon tests use — the full stack a TCP conn
//! traverses (gate → public router → `/v1` router), no socket bound. The `gate`,
//! `headers`, and `routes` tests import their helpers from here.

use super::*;
use crate::daemon::{App, Transport, handler};
use crate::persist::Log;
use trillium::Method;
use trillium_testing::TestServer;

/// A throwaway state database path, so tests never write the real user-scoped file.
pub(super) fn scratch_state() -> PathBuf {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    std::env::temp_dir().join(format!("efference-tcp-test-{n:016x}/state.db"))
}

/// An armed plaintext-loopback bind under a per-test root, over `state` (a distinct
/// root per test keeps one test's burns invisible to another's codes).
pub(super) fn armed_over(root: &str, state: PathBuf) -> Arc<TcpAuth> {
    let (auth, _listener) = arm_with_state(
        TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            public_url: "http://localhost:7777".into(),
            root: Root::new(root),
            open: false,
        },
        state,
    );
    auth
}

/// [`armed_over`] a scratch state database of its own.
pub(super) fn armed(root: &str) -> Arc<TcpAuth> {
    armed_over(root, scratch_state())
}

/// An `Authorization` value for a script enrolled at `auth`, through the same
/// enrollment the socket's `POST /auth/devices` runs.
pub(super) fn enrolled(auth: &TcpAuth) -> String {
    let enrolled = auth
        .enroll(Kind::Script, "test", Joined::Add)
        .expect("a scratch database takes a row");
    format!("Bearer {}", enrolled.token)
}

fn init(shared: Arc<Session>) -> impl trillium::Handler {
    trillium::Init::new(move |info: trillium::Info| {
        let shared = Arc::clone(&shared);
        async move { info.with_shared_state(shared) }
    })
}

/// The web stack `serve --bind` mounts, from `handler` itself rather than a copy of it,
/// virtually hosted at the bind's public name so the gate's Host check sees what a
/// real conn carries.
pub(super) async fn tcp_server(
    auth: &Arc<TcpAuth>,
) -> (TestServer<impl trillium::Handler>, TurnsGuard) {
    let (shared, turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let server = TestServer::new((
        init(shared),
        handler(
            Transport::Web {
                auth: Arc::clone(auth),
            },
            App::session(std::env::current_dir().unwrap()),
        ),
    ))
    .await
    .with_host("localhost");
    (server, TurnsGuard(turns_rx))
}

/// Keeps the turn-inbox receiver alive for the server's lifetime, so a routed
/// `POST /turns` has a live channel to land on.
pub(super) struct TurnsGuard(#[allow(dead_code)] mpsc::UnboundedReceiver<SubmittedTurn>);

/// The six digits currently valid for this auth's derived TOTP secret.
pub(super) fn current_code(auth: &TcpAuth) -> String {
    auth::format_code(auth::totp(
        &auth.totp_secret,
        auth::timestep(std::time::SystemTime::now()),
    ))
}

/// A device token authenticates in the `Authorization` header and nowhere else. A
/// cookie and a query parameter are what a request another site forces could
/// carry — a navigation, a form, an image — so the same value there is refused
/// (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_authorization_header_carries_a_token() {
    let auth = armed("token-authenticates");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = enrolled(&auth);
    let token = bearer.strip_prefix("Bearer ").unwrap().to_string();

    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(200);

    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, "Bearer not-a-token")
        .await
        .assert_status(401);
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, token.clone())
        .await
        .assert_status(401);
    for name in ["efference-session", "__Host-efference", "token"] {
        server
            .get("/v1/state")
            .with_request_header(KnownHeaderName::Cookie, format!("{name}={token}"))
            .await
            .assert_status(401);
    }
    for query in ["token", "access_token", "bearer"] {
        server
            .get(&format!("/v1/state?{query}={token}"))
            .await
            .assert_status(401);
    }
}

/// No answer the bind gives sets a cookie: the credential leaves the mint in a
/// body, and nothing else on the stack writes one (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_answer_sets_a_cookie() {
    let auth = armed("no-set-cookie");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = enrolled(&auth);
    let answers = [
        server
            .post("/auth/session")
            .with_json_body(&serde_json::json!({ "code": current_code(&auth) }))
            .await,
        server
            .post("/auth/session")
            .with_json_body(&serde_json::json!({ "code": "not a code" }))
            .await,
        server
            .get("/v1/state")
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .await,
        server.get("/v1/state").await,
        server
            .get("/files/")
            .with_request_header(KnownHeaderName::Authorization, bearer)
            .await,
        server.get("/").await,
    ];
    for (n, answer) in answers.iter().enumerate() {
        assert!(
            answer.header(KnownHeaderName::SetCookie).is_none(),
            "answer {n} ({:?}) set a cookie",
            answer.status()
        );
    }
}

/// The bind grants no other origin anything: a cross-origin read and a CORS
/// preflight both answer without an `Access-Control-Allow-*` header, so a page
/// on another origin — another port on `localhost` included — can neither read
/// an answer nor send the header a token rides (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_answer_grants_another_origin() {
    let auth = armed("no-cors");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = enrolled(&auth);
    let answers = [
        server
            .get("/v1/state")
            .with_request_header(KnownHeaderName::Origin, "http://localhost:3000")
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .await,
        server
            .get("/v1/state")
            .with_request_header(KnownHeaderName::Origin, "https://evil.example")
            .await,
        // The preflight a cross-origin `fetch` carrying the header would send
        // first; browsers attach no credential to it.
        server
            .build(Method::Options, "/v1/state")
            .with_request_header(KnownHeaderName::Origin, "http://localhost:3000")
            .with_request_header(KnownHeaderName::AccessControlRequestMethod, "GET")
            .with_request_header(
                KnownHeaderName::AccessControlRequestHeaders,
                "authorization",
            )
            .await,
        server
            .build(Method::Options, "/auth/session")
            .with_request_header(KnownHeaderName::Origin, "https://evil.example")
            .with_request_header(KnownHeaderName::AccessControlRequestMethod, "POST")
            .with_request_header(KnownHeaderName::AccessControlRequestHeaders, "content-type")
            .await,
        server
            .build(Method::Options, "/v1/state")
            .with_request_header(KnownHeaderName::Origin, "http://localhost:3000")
            .with_request_header(KnownHeaderName::Authorization, bearer)
            .await,
    ];
    for (n, answer) in answers.iter().enumerate() {
        let granted: Vec<String> = answer
            .response_headers()
            .iter()
            .map(|(name, _)| name.as_ref().to_ascii_lowercase())
            .filter(|name| name.starts_with("access-control-"))
            .collect();
        assert!(
            granted.is_empty(),
            "answer {n} ({:?}) granted {granted:?}",
            answer.status()
        );
    }
}

#[test]
fn host_and_origin_parsing() {
    assert_eq!(host_only("example.ts.net:8443"), "example.ts.net");
    assert_eq!(host_only("example.ts.net"), "example.ts.net");
    assert_eq!(host_only("[::1]:8443"), "[::1]");
    assert_eq!(host_only("[::1]"), "[::1]");
    assert_eq!(origin_host("https://example.ts.net:8443"), "example.ts.net");
    assert_eq!(origin_host("http://localhost:3000"), "localhost");
    assert_eq!(origin_host("null"), "null");
}

/// The arm records whether its listener terminates TLS, which is what HSTS
/// reads (docs/web-bind.md); a plaintext loopback bind is not TLS.
#[test]
fn only_a_tls_arm_is_tls() {
    let (tls, _listener) = arm_with_state(
        TcpBind {
            addr: "0.0.0.0:0".into(),
            tls: Some(TlsPem {
                cert: Vec::new(),
                key: Vec::new(),
            }),
            public_url: "https://host.example:8443".into(),
            root: Root::new("tls-arm"),
            open: false,
        },
        scratch_state(),
    );
    assert!(tls.is_tls());
    assert!(!armed("plain-arm").is_tls());
}

/// A bind to port 0 mints links naming the port it actually bound; a configured
/// public URL with its own port is left as given.
#[test]
fn a_port_zero_bind_mints_links_on_its_bound_port() {
    let (auth, _listener) = arm_with_state(
        TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            public_url: "http://localhost:0".into(),
            root: Root::new("port-zero"),
            open: false,
        },
        scratch_state(),
    );
    auth.bound_port(51234);
    assert_eq!(auth.public_base(), "http://localhost:51234");
    assert!(
        auth.mint_link()
            .unwrap()
            .starts_with("http://localhost:51234/#token=")
    );
    let configured = armed("configured-port");
    configured.bound_port(51234);
    assert_eq!(configured.public_base(), "http://localhost:7777");
}

/// A second link minted in the same 30 seconds as a redeemed one carries the next
/// step's code, not the burned one; once every accepted step is used, none is
/// minted rather than a dead one.
#[test]
fn a_minted_link_never_carries_a_used_code() {
    let auth = armed("mint-unburned");
    let code_of = |link: String| link.rsplit_once("token=").unwrap().1.to_string();
    let first = code_of(auth.mint_link().expect("nothing burned yet"));
    let now = auth::timestep(SystemTime::now());
    auth.ledger.burn(now).unwrap();
    let second = code_of(auth.mint_link().expect("the next step is unused"));
    assert_ne!(first, second);
    assert_eq!(
        second,
        auth::format_code(auth::totp(&auth.totp_secret, now + 1))
    );
    auth.ledger.burn(now + 1).unwrap();
    assert_eq!(auth.mint_link(), None);
}

/// The tag is checked before the row, and the row is the answer: a token this root
/// minted for an id with no row is refused, and so is an enrolled device's token
/// once its row is deleted, on the very next request (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_without_a_row_is_refused() {
    let auth = armed("row-is-the-answer");
    let (server, _guard) = tcp_server(&auth).await;

    let rowless = auth.tokens.token(&devices::DeviceId::random());
    assert!(auth.tokens.parse(&rowless).is_some(), "the tag verifies");
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, format!("Bearer {rowless}"))
        .await
        .assert_status(401);

    let device = auth.enroll(Kind::Browser, "phone", Joined::Code).unwrap();
    let bearer = format!("Bearer {}", device.token);
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .await
        .assert_status(200);
    assert!(auth.devices.remove(&device.device.id).unwrap());
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(401)
        .assert_body_contains(r#""code":"unauthorized""#);
}

/// A device table that cannot be read refuses a verified token with a `500` rather
/// than a `401`, which a client reads as a bad token and discards. A malformed or
/// forged token is still a `401`: it is refused before the table is consulted, which
/// is what makes its answer the same whatever state the table is in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_table_refuses_without_discarding_the_token() {
    let state = scratch_state();
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, "{ not a database").unwrap();
    let auth = armed_over("unreadable-table", state);
    let (server, _guard) = tcp_server(&auth).await;

    let verified = auth.tokens.token(&devices::DeviceId::random());
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, format!("Bearer {verified}"))
        .await
        .assert_status(500)
        .assert_body_contains(r#""code":"internal""#);

    let mut forged = verified.clone();
    forged.replace_range(forged.len() - 1.., "A");
    if forged == verified {
        forged.replace_range(forged.len() - 1.., "B");
    }
    for token in [forged.as_str(), "efference_short", "not-a-token"] {
        server
            .get("/v1/state")
            .with_request_header(KnownHeaderName::Authorization, format!("Bearer {token}"))
            .await
            .assert_status(401);
    }
    // The shell stays free: an anonymous path never reaches the table.
    server.get("/").await.assert_status(200);
}
