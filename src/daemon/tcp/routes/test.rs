//! Tests for the sign-in routes and the enrollment routes (docs/web-bind.md). The
//! helpers are in `tcp/test.rs`.

use super::super::test::{armed, enrolled, link_claim, tcp_server};
use super::*;
use crate::devices::{Device, Joined, Kind};
use trillium_testing::TestServer;

/// The body of an answer, as JSON.
fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

/// The whole flow, as a browser and an operator see it: the page asks and gets a code
/// and a claim, the claim waits, the operator approves the code over the socket as they
/// typed it, and the next claim collects a token — once — for a `browser` device under
/// the page's cleaned label; the token is the credential the gate takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_signs_in_by_approval_at_the_host() {
    let (auth, _state) = armed("sign-in-by-approval");
    let (server, _guard) = tcp_server(&auth).await;
    let socket = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;

    let asked = server
        .post("/pair")
        .with_json_body(&serde_json::json!({ "label": "Firefox\u{1b}[2J on\nLinux" }))
        .await;
    asked.assert_status(200);
    assert_eq!(
        asked
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("no-store")
    );
    let asked = json(asked.body());
    let claim = asked["claim"].as_str().expect("a claim").to_string();
    let code = asked["code"].as_str().expect("a code").to_string();
    assert_eq!(asked["expires_in"], TTL_SECS);
    assert!(
        ShortCode::parse(&code).is_some_and(|parsed| parsed.to_string() == code),
        "displayed as it parses: {code}"
    );
    let claim_body = serde_json::json!({ "claim": claim, "label": "a page's own name" });

    server
        .post("/pair/claim")
        .with_json_body(&claim_body)
        .await
        .assert_status(202)
        .assert_body(r#"{"status":"pending"}"#);

    let waiting = socket.get("/auth/pending").await;
    waiting.assert_status(200);
    let listed = json(waiting.body());
    assert_eq!(listed["pending"][0]["label"], "Firefox on Linux");
    assert!(!waiting.body().contains(&code), "{}", waiting.body());
    assert!(
        !waiting.body().contains(&code.replace('-', "")),
        "{}",
        waiting.body()
    );

    // Typed the way a person types it: lower case, a space for the dash.
    let typed = code.to_lowercase().replace('-', " ");
    let approved = socket
        .post("/auth/approve")
        .with_json_body(&serde_json::json!({ "code": typed }))
        .await;
    approved.assert_status(200);
    let approved = json(approved.body());
    assert_eq!(approved["label"], "Firefox on Linux");
    assert!(approved["created_at"].is_i64(), "{approved}");
    socket
        .get("/auth/pending")
        .await
        .assert_body(r#"{"pending":[]}"#);

    let collected = server.post("/pair/claim").with_json_body(&claim_body).await;
    collected.assert_status(200);
    assert_eq!(
        collected
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("no-store")
    );
    assert!(collected.header(KnownHeaderName::SetCookie).is_none());
    let token = json(collected.body())["token"]
        .as_str()
        .expect("a token in the body")
        .to_string();

    let devices = auth.devices.list().unwrap();
    let [device] = devices.as_slice() else {
        panic!("one device enrolled: {devices:?}");
    };
    assert_eq!(
        (device.kind, device.joined, device.label.as_str()),
        (Kind::Browser, Joined::Code, "Firefox on Linux"),
        "an approved request keeps the label the operator approved"
    );
    assert_eq!(auth.tokens.parse(&token), Some(device.id));
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, format!("Bearer {token}"))
        .await
        .assert_status(200);

    // Spent: the second claim and a second approval find nothing.
    server
        .post("/pair/claim")
        .with_json_body(&claim_body)
        .await
        .assert_status(404)
        .assert_body_contains(r#""code":"not_found""#);
    socket
        .post("/auth/approve")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await
        .assert_status(404);
    assert_eq!(auth.devices.list().unwrap().len(), 1);
}

/// The link is a sign-in approved before it was asked for: the page claims it at once,
/// enrolling a device that joined by `link` under the label the page sends, and the
/// link's claim collects nothing after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_link_claims_once_as_a_link() {
    let (auth, _state) = armed("link-claims");
    let (server, _guard) = tcp_server(&auth).await;
    let socket = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;

    let minted = socket.get("/auth/link").await;
    minted.assert_status(200);
    assert_eq!(
        minted
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("no-store")
    );
    let url = json(minted.body())["url"].as_str().unwrap().to_string();
    let claim = url
        .strip_prefix("http://localhost:7777/#claim=")
        .unwrap_or_else(|| panic!("the public base, the claim in the fragment: {url}"));
    socket
        .get("/auth/pending")
        .await
        .assert_body(r#"{"pending":[]}"#);

    let body = serde_json::json!({ "claim": claim, "label": "Safari on\u{7} iOS" });
    let collected = server.post("/pair/claim").with_json_body(&body).await;
    collected.assert_status(200);
    let token = json(collected.body())["token"]
        .as_str()
        .unwrap()
        .to_string();
    let devices = auth.devices.list().unwrap();
    assert_eq!(
        (
            devices[0].kind,
            devices[0].joined,
            devices[0].label.as_str()
        ),
        (Kind::Browser, Joined::Link, "Safari on iOS")
    );
    assert_eq!(auth.tokens.parse(&token), Some(devices[0].id));
    server
        .post("/pair/claim")
        .with_json_body(&body)
        .await
        .assert_status(404);

    // A page that sends no label is a device called `browser`. Both rows were made
    // in the same second, so the listing's order between them is the ids'.
    server
        .post("/pair/claim")
        .with_json_body(&serde_json::json!({ "claim": link_claim(&auth) }))
        .await
        .assert_status(200);
    let mut labels: Vec<String> = auth
        .devices
        .list()
        .unwrap()
        .into_iter()
        .map(|device| device.label)
        .collect();
    labels.sort();
    assert_eq!(labels, ["Safari on iOS", "browser"]);
}

/// The sign-in routes are anonymous, not unguarded: they take the gate's Host, Origin,
/// and Content-Type checks like any unsafe request, so another site can neither start
/// a sign-in through the operator's browser nor collect one, and nothing is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sign_in_routes_take_the_gates_checks() {
    let (auth, _state) = armed("sign-in-gate-checks");
    let (server, _guard) = tcp_server(&auth).await;
    for path in ["/pair", "/pair/claim"] {
        // Refused before any body is read, so these carry none: one still being
        // written races the in-memory connection's close.
        server
            .post(path)
            .with_request_header(KnownHeaderName::Origin, "https://evil.example")
            .with_request_header(KnownHeaderName::ContentType, "application/json")
            .await
            .assert_status(403);
        server
            .post(path)
            .with_request_header(KnownHeaderName::Host, "attacker.example")
            .with_request_header(KnownHeaderName::ContentType, "application/json")
            .await
            .assert_status(403);
        server
            .post(path)
            .with_request_header(
                KnownHeaderName::ContentType,
                "application/x-www-form-urlencoded",
            )
            .with_body("label=phone")
            .await
            .assert_status(400);
        server
            .post(path)
            .with_request_header(KnownHeaderName::ContentType, "text/plain")
            .with_body(r#"{"label":"phone"}"#)
            .await
            .assert_status(400);
    }
    assert!(auth.sign_ins.waiting(SystemTime::now()).unwrap().is_empty());
    // The same request from the page's own origin is taken.
    server
        .post("/pair")
        .with_request_header(KnownHeaderName::Origin, "http://localhost:7777")
        .with_json_body(&serde_json::json!({ "label": "phone" }))
        .await
        .assert_status(200);
    assert_eq!(auth.sign_ins.waiting(SystemTime::now()).unwrap().len(), 1);
}

/// A request that sends no label is a sign-in called `browser`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_without_a_label_is_called_browser() {
    let (auth, _state) = armed("unlabelled-request");
    let (server, _guard) = tcp_server(&auth).await;
    server
        .post("/pair")
        .with_json_body(&serde_json::json!({}))
        .await
        .assert_status(200);
    assert_eq!(
        auth.sign_ins.waiting(SystemTime::now()).unwrap()[0].label,
        "browser"
    );
}

/// What names nothing is refused: a claim that is not a claim secret and one no sign-in
/// answers to are the same `404`, a code that names no waiting sign-in is a `404`, and
/// what is not a code at all is a `400`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_names_no_pending_sign_in_is_refused() {
    let (auth, _state) = armed("names-nothing");
    let (server, _guard) = tcp_server(&auth).await;
    let socket = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;

    let unknown = server
        .post("/pair/claim")
        .with_json_body(&serde_json::json!({ "claim": "AAAAAAAAAAAAAAAAAAAAAA" }))
        .await;
    unknown.assert_status(404);
    let malformed = server
        .post("/pair/claim")
        .with_json_body(&serde_json::json!({ "claim": "../../etc/passwd" }))
        .await;
    malformed.assert_status(404);
    assert_eq!(unknown.body(), malformed.body(), "one answer for both");
    server
        .post("/pair/claim")
        .with_json_body(&serde_json::json!({ "token": "x" }))
        .await
        .assert_status(400);

    socket
        .post("/auth/approve")
        .with_json_body(&serde_json::json!({ "code": "ABC-DEF" }))
        .await
        .assert_status(404)
        .assert_body_contains("no sign-in is waiting under that code");
    socket
        .post("/auth/approve")
        .with_json_body(&serde_json::json!({ "code": "123456" }))
        .await
        .assert_status(400)
        .assert_body_contains("six letters and digits");
    assert!(auth.devices.list().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_routes_answer_only_when_a_bind_is_armed() {
    let (auth, _state) = armed("host-routes");
    let armed_server = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;
    armed_server
        .get("/auth/link")
        .await
        .assert_status(200)
        .assert_body_contains("/#claim=");
    armed_server
        .get("/auth/pending")
        .await
        .assert_status(200)
        .assert_body(r#"{"pending":[]}"#);
    armed_server
        .get("/auth/devices")
        .await
        .assert_status(200)
        .assert_body(r#"{"devices":[]}"#);

    let unarmed = TestServer::new(enrollment_routes(None)).await;
    unarmed.get("/auth/link").await.assert_status(404);
    unarmed.get("/auth/pending").await.assert_status(404);
    unarmed
        .post("/auth/approve")
        .with_json_body(&serde_json::json!({ "code": "KQ7-M4X" }))
        .await
        .assert_status(404)
        .assert_body_contains("no TCP bind armed");
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
    let (auth, _state) = armed("script-enrolls");
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
    let enrolled: Enrolled = serde_json::from_str(added.body()).unwrap();
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
    let listed: Value = serde_json::from_str(listing.body()).unwrap();
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

/// The enrollment routes are the socket's alone: over TCP they are no route at all,
/// with a credential or without one, and nothing they would do happens — no link is
/// minted, no code approved, no device added or revoked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_enrollment_routes_are_absent_from_tcp() {
    let (auth, _state) = armed("enrollment-socket-only");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = enrolled(&auth);
    let id = auth.devices.list().unwrap()[0].id;
    let one = format!("/auth/devices/{id}");
    let requested = auth.sign_ins.request("waiting", SystemTime::now()).unwrap();

    for path in ["/auth/link", "/auth/pending", "/auth/devices"] {
        server
            .get(path)
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .await
            .assert_status(404);
        server.get(path).await.assert_status(401);
    }
    // JSON-typed to pass the gate's content-type check, and bodiless: nothing past
    // the gate reads a body here, and one still being written races the in-memory
    // connection's close.
    for path in ["/auth/approve", "/auth/devices"] {
        server
            .post(path)
            .with_request_header(KnownHeaderName::Authorization, bearer.clone())
            .with_request_header(KnownHeaderName::ContentType, "application/json")
            .await
            .assert_status(404);
        server
            .post(path)
            .with_request_header(KnownHeaderName::ContentType, "application/json")
            .await
            .assert_status(401);
    }
    server
        .delete(&one)
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .await
        .assert_status(404);
    server.delete(&one).await.assert_status(401);

    let devices = auth.devices.list().unwrap();
    assert_eq!(devices.len(), 1, "nothing was added: {devices:?}");
    assert_eq!(devices[0].id, id, "nothing was revoked");
    assert_eq!(
        auth.sign_ins
            .claim(&requested.claim, SystemTime::now())
            .unwrap(),
        Claim::Waiting,
        "nothing was approved"
    );
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(200);
}

/// The TOTP surface is gone: with a credential, every one of its routes is no route
/// on either listener, and without one, the TCP gate prices them like any other path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_totp_routes_answer_404() {
    let (auth, _state) = armed("totp-gone");
    let (server, _guard) = tcp_server(&auth).await;
    let socket = TestServer::new(enrollment_routes(Some(Arc::clone(&auth)))).await;
    let bearer = enrolled(&auth);

    server
        .post("/auth/session")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::ContentType, "application/json")
        .await
        .assert_status(404);
    server
        .get("/auth/otpauth")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(404);
    server
        .post("/auth/session")
        .with_request_header(KnownHeaderName::ContentType, "application/json")
        .await
        .assert_status(401);
    socket.get("/auth/otpauth").await.assert_status(404);
    socket
        .post("/auth/session")
        .with_request_header(KnownHeaderName::ContentType, "application/json")
        .await
        .assert_status(404);
}
