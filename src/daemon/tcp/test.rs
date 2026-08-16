//! Tests for the TCP bind's gate and routes (docs/web-bind.md), over the same
//! in-memory transport the other daemon tests use — the full stack a TCP conn
//! traverses (gate → public router → `/v1` router), no socket bound.

use super::*;
use crate::persist::Log;
use trillium_testing::TestServer;

/// A throwaway burn-ledger path, so tests never write the real user-scoped file.
fn scratch_ledger() -> BurnLedger {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    BurnLedger::new(std::env::temp_dir().join(format!("efference-tcp-test-{n:016x}")))
}

/// An armed plaintext-loopback bind under a per-test root (per-test ledger, and a
/// distinct root per test keeps one test's burns invisible to another's codes).
fn armed(root: &str) -> Arc<TcpAuth> {
    let (auth, _listener) = arm_with_ledger(
        TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            host: "localhost".into(),
            root: Root::new(root),
            open: false,
        },
        scratch_ledger(),
    );
    auth
}

fn init(shared: Arc<Shared>) -> impl trillium::Handler {
    trillium::Init::new(move |info: trillium::Info| {
        let shared = Arc::clone(&shared);
        async move { info.with_shared_state(shared) }
    })
}

/// The exact handler stack `spawn` mounts on the TCP listener, virtually hosted at
/// the bind's public name so the gate's Host check sees what a real conn carries.
async fn tcp_server(auth: &Arc<TcpAuth>) -> (TestServer<impl trillium::Handler>, TurnsGuard) {
    let (shared, turns_rx) = Shared::new(Log::in_memory());
    let server = TestServer::new((
        init(shared),
        trillium_head::head(),
        gate(Arc::clone(auth)),
        public_router(Arc::clone(auth)),
        router(),
    ))
    .await
    .with_host("localhost");
    (server, TurnsGuard(turns_rx))
}

/// Keeps the turn-inbox receiver alive for the server's lifetime, so a routed
/// `POST /turns` has a live channel to land on.
struct TurnsGuard(#[allow(dead_code)] mpsc::UnboundedReceiver<String>);

/// The six digits currently valid for this auth's derived TOTP secret.
fn current_code(auth: &TcpAuth) -> String {
    auth::format_code(auth::totp(
        &auth.totp_secret,
        auth::timestep(std::time::SystemTime::now()),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bootstrap_page_is_anonymous_and_the_api_is_not() {
    let auth = armed("page-vs-api");
    let (server, _guard) = tcp_server(&auth).await;

    server
        .get("/")
        .await
        .assert_status(200)
        .assert_header_with(KnownHeaderName::ContentType, |ct| {
            assert!(ct.as_str().is_some_and(|ct| ct.starts_with("text/html")))
        })
        // The bootstrap JS is served: the page is what reads the fragment token.
        .assert_body_contains("#token");

    // HEAD rides `trillium_head::head()`: 200 like the GET, with the body stripped.
    let head = server.build(Method::Head, "/").await;
    head.assert_status(200);
    assert_eq!(head.body(), "", "HEAD carries no body");

    server
        .get("/v1/state")
        .await
        .assert_status(401)
        .assert_body_contains(r#""code":"unauthorized""#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_current_code_mints_a_cookie_and_burns() {
    let auth = armed("code-mints-cookie");
    let (server, _guard) = tcp_server(&auth).await;
    let code = current_code(&auth);

    let session = server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await;
    session.assert_status(200);
    let set_cookie = session
        .header(KnownHeaderName::SetCookie)
        .expect("a session cookie was set")
        .to_string();
    assert!(set_cookie.starts_with(auth::COOKIE_NAME));
    for attribute in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(
            set_cookie.contains(attribute),
            "{attribute} in {set_cookie}"
        );
    }

    // The minted cookie authenticates the API.
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Cookie, cookie)
        .await
        .assert_status(200);

    // The accepted code burned: the same six digits cannot mint twice.
    server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await
        .assert_status(401)
        .assert_body_contains("already used");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bearer_authenticates_programs() {
    let auth = armed("bearer-authenticates");
    let (server, _guard) = tcp_server(&auth).await;

    server
        .get("/v1/state")
        .with_request_header(
            KnownHeaderName::Authorization,
            format!("Bearer {}", auth.bearer()),
        )
        .await
        .assert_status(200);

    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, "Bearer not-the-bearer")
        .await
        .assert_status(401);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_codes_lock_verification_out() {
    let auth = armed("lockout-engages");
    let (server, _guard) = tcp_server(&auth).await;

    // "000000" is not this root's live code (deterministic secret), so each attempt
    // is a failure; the sixth lands inside the lockout.
    for _ in 0..5 {
        server
            .post("/auth/session")
            .with_json_body(&serde_json::json!({ "code": "000000" }))
            .await
            .assert_status(401);
    }
    server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": "000000" }))
        .await
        .assert_status(429)
        .assert_header_with(KnownHeaderName::RetryAfter, |v| {
            assert!(v.as_str().is_some_and(|s| s.parse::<u64>().is_ok()))
        });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsafe_methods_enforce_origin_and_content_type() {
    let auth = armed("csrf-hardening");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = format!("Bearer {}", auth.bearer());

    // A cross-site Origin is rejected even with a valid credential.
    server
        .post("/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::Origin, "https://evil.example")
        .with_json_body(&serde_json::json!({ "text": "hi" }))
        .await
        .assert_status(403)
        .assert_body_contains(r#""code":"forbidden""#);

    // A same-origin browser request passes (any port: cookies aren't port-scoped
    // and neither is the allowlist).
    server
        .post("/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::Origin, "http://localhost:7777")
        .with_json_body(&serde_json::json!({ "text": "hi" }))
        .await
        .assert_status(202);

    // No JSON content-type on a body-bearing method: the no-preflight CSRF shape.
    server
        .post("/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .with_request_header(
            KnownHeaderName::ContentType,
            "application/x-www-form-urlencoded",
        )
        .with_body("text=hi")
        .await
        .assert_status(400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_host_is_rejected() {
    let auth = armed("host-allowlist");
    let (server, _guard) = tcp_server(&auth).await;

    server
        .get("/")
        .with_request_header(KnownHeaderName::Host, "attacker.example")
        .await
        .assert_status(403)
        .assert_body_contains(r#""code":"forbidden""#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_routes_answer_only_when_a_bind_is_armed() {
    let auth = armed("host-routes");
    let armed_server = TestServer::new(host_router(Some(Arc::clone(&auth)))).await;
    armed_server
        .get("/auth/link")
        .await
        .assert_status(200)
        .assert_body_contains("/#token=");
    armed_server
        .get("/auth/otpauth")
        .await
        .assert_body_contains("otpauth://totp/");
    armed_server
        .get("/auth/bearer")
        .await
        .assert_body_contains(r#""bearer":"#);

    let unarmed = TestServer::new(host_router(None)).await;
    unarmed.get("/auth/link").await.assert_status(404);
}

/// An h2 conn carries its authority in the `:authority` pseudo-header and no Host
/// header at all — the shape every browser produces against the TLS bind, since the
/// acceptor's ALPN negotiates h2. The gate must read it (`Conn::host()` alone does
/// not, trillium-http 1.6). Exercised at the handler level: `TestConn` speaks h1, so
/// the pseudo-header is set directly on the inner conn.
#[test]
fn an_h2_authority_satisfies_the_host_check() {
    let auth = armed("h2-authority");
    let gate = gate(Arc::clone(&auth));

    let mut conn = trillium_testing::TestConn::build(Method::Get, "/", ());
    let inner: &mut trillium_http::Conn<_> = conn.as_mut();
    assert_eq!(inner.host(), None, "no Host header on a bare TestConn");
    inner.set_authority(Some("localhost:7777"));
    let conn = conn.run(&gate);
    assert!(!conn.is_halted(), "the authority passed the allowlist");

    // And the failure this regressed as: authority absent everywhere → 403.
    let conn = trillium_testing::TestConn::build(Method::Get, "/", ()).run(&gate);
    assert!(conn.is_halted());
    assert_eq!(conn.status().map(|s| s as u16), Some(403));
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
