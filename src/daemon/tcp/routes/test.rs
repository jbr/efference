//! Tests for the credential mint and the enrollment routes (docs/web-bind.md). The helpers
//! are in `tcp/test.rs`.

use super::super::test::{armed, current_code, tcp_server};
use super::*;
use trillium_testing::TestServer;

/// The mint answers a current code with the bearer in its body — never a cookie,
/// and never stored for reuse — and the token it answers is the credential the
/// gate takes (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_current_code_mints_the_bearer_and_burns() {
    let auth = armed("code-mints-bearer");
    let (server, _guard) = tcp_server(&auth).await;
    let code = current_code(&auth);

    let session = server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await;
    session.assert_status(200);
    assert!(
        session.header(KnownHeaderName::SetCookie).is_none(),
        "the mint sets no cookie"
    );
    assert_eq!(
        session
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("no-store")
    );
    let body: serde_json::Value = serde_json::from_str(session.body()).unwrap();
    let token = body["token"].as_str().expect("a token in the body");
    assert_eq!(token, auth.bearer());

    // The minted token authenticates the API, and its absence does not.
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, format!("Bearer {token}"))
        .await
        .assert_status(200);
    server.get("/v1/state").await.assert_status(401);

    // The accepted code burned: the same six digits cannot mint twice.
    server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await
        .assert_status(401)
        .assert_body_contains("already used");
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
async fn the_host_routes_answer_only_when_a_bind_is_armed() {
    let auth = armed("host-routes");
    let armed_server = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;
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

    let unarmed = TestServer::new(enrollment_routes(None)).await;
    unarmed.get("/auth/link").await.assert_status(404);
}
