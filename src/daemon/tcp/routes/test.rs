//! Tests for the credential mint and the enrollment routes (docs/web-bind.md). The helpers
//! are in `tcp/test.rs`.

use super::super::test::{armed, current_code, enrolled, tcp_server};
use super::*;
use crate::devices::{Device, Joined, Kind};
use trillium_testing::TestServer;

/// The mint answers a current code with a token in its body — never a cookie, and
/// never stored for reuse — for a `browser` device it enrolls under the page's label,
/// cleaned of what would steer a terminal; and the token is the credential the gate
/// takes (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_current_code_enrolls_a_browser_and_answers_its_token() {
    let auth = armed("code-enrolls-browser");
    let (server, _guard) = tcp_server(&auth).await;
    let code = current_code(&auth);

    let session = server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({
            "code": code,
            "label": "Firefox\u{1b}[2J on\nLinux",
        }))
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

    let devices = auth.devices.list().unwrap();
    let [device] = devices.as_slice() else {
        panic!("one device enrolled: {devices:?}");
    };
    assert_eq!(
        (device.kind, device.joined, device.label.as_str()),
        (Kind::Browser, Joined::Code, "Firefox [2J on Linux")
    );
    assert_eq!(auth.tokens.parse(token), Some(device.id));

    // The token authenticates the API, and its absence does not.
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, format!("Bearer {token}"))
        .await
        .assert_status(200);
    server.get("/v1/state").await.assert_status(401);

    // The accepted code burned: the same six digits cannot enroll twice.
    server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await
        .assert_status(401)
        .assert_body_contains("already used");
    assert_eq!(auth.devices.list().unwrap().len(), 1);
}

/// A page that sends no label is a device called `browser`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_without_a_label_is_called_browser() {
    let auth = armed("unlabelled-browser");
    let (server, _guard) = tcp_server(&auth).await;
    server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": current_code(&auth) }))
        .await
        .assert_status(200);
    assert_eq!(auth.devices.list().unwrap()[0].label, "browser");
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
    assert!(auth.devices.list().unwrap().is_empty());
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
        .get("/auth/devices")
        .await
        .assert_status(200)
        .assert_body(r#"{"devices":[]}"#);

    let unarmed = TestServer::new(enrollment_routes(None)).await;
    unarmed.get("/auth/link").await.assert_status(404);
    unarmed
        .get("/auth/devices")
        .await
        .assert_status(404)
        .assert_body_contains("no TCP bind armed");
}

/// A script enrolled over the socket gets a token once, which the TCP listener then
/// takes; the listing shows the device and never the token; revoking it refuses the
/// token on the next request (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_script_enrolled_at_the_socket_authenticates_until_revoked() {
    let auth = armed("script-enrolls");
    let socket = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;
    let (server, _guard) = tcp_server(&auth).await;

    let added = socket
        .post("/auth/devices")
        .with_json_body(&serde_json::json!({ "label": "deploy\u{7}bot" }))
        .await;
    added.assert_status(201);
    assert_eq!(
        added
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("no-store")
    );
    let enrolled: crate::devices::Enrolled = serde_json::from_str(added.body()).unwrap();
    assert_eq!(
        (enrolled.device.kind, enrolled.device.joined),
        (Kind::Script, Joined::Add)
    );
    assert_eq!(enrolled.device.label, "deploy bot");
    let bearer = format!("Bearer {}", enrolled.token);

    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .await
        .assert_status(200);

    let listing = socket.get("/auth/devices").await;
    listing.assert_status(200);
    assert!(
        !listing.body().contains(&enrolled.token),
        "the listing never carries a token"
    );
    let listed: serde_json::Value = serde_json::from_str(listing.body()).unwrap();
    let listed: Vec<Device> = serde_json::from_value(listed["devices"].clone()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, enrolled.device.id);
    assert!(
        listed[0].last_seen_at.is_some(),
        "the request above recorded it seen"
    );

    let path = format!("/auth/devices/{}", enrolled.device.id);
    socket
        .delete(&path)
        .await
        .assert_status(200)
        .assert_body_contains(&enrolled.device.id.hex());
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(401);
    socket.delete(&path).await.assert_status(404);
    socket
        .delete("/auth/devices/not-an-id")
        .await
        .assert_status(400);
}

/// The device routes are the socket's alone: over TCP they are no route at all, with
/// a credential or without one, and nothing they would do happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_device_routes_are_absent_from_tcp() {
    let auth = armed("devices-socket-only");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = enrolled(&auth);
    let id = auth.devices.list().unwrap()[0].id;
    let one = format!("/auth/devices/{id}");

    server
        .get("/auth/devices")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .await
        .assert_status(404);
    // JSON-typed to pass the gate's content-type check, and bodiless: nothing past
    // the gate reads a body here, and one still being written races the in-memory
    // connection's close.
    server
        .post("/auth/devices")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::ContentType, "application/json")
        .await
        .assert_status(404);
    server
        .delete(&one)
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .await
        .assert_status(404);
    for anonymous in [server.get("/auth/devices").await, server.delete(&one).await] {
        anonymous.assert_status(401);
    }

    let devices = auth.devices.list().unwrap();
    assert_eq!(devices.len(), 1, "nothing was added: {devices:?}");
    assert_eq!(devices[0].id, id, "nothing was revoked");
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(200);
}
