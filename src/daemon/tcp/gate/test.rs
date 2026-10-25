//! Tests for the gate and the anonymous surface it prices (docs/web-bind.md). The helpers
//! are in `tcp/test.rs`.

use super::super::test::{armed, enrolled, tcp_server};
use super::*;
use trillium_testing::TestServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_web_client_is_anonymous_and_the_api_is_not() {
    let auth = armed("page-vs-api");
    let (server, _guard) = tcp_server(&auth).await;

    // The SPA shell (docs/web-client.md): served without a credential, like the
    // inline bootstrap page it replaced — the login view is inside the app.
    server
        .get("/")
        .await
        .assert_status(200)
        .assert_header_with(KnownHeaderName::ContentType, |ct| {
            assert!(ct.as_str().is_some_and(|ct| ct.starts_with("text/html")))
        });

    // HEAD rides `trillium_head::head()`: 200 like the GET, with the body stripped.
    let head = server.build(Method::Head, "/").await;
    head.assert_status(200);
    assert_eq!(head.body(), "", "HEAD carries no body");

    server
        .get("/v1/state")
        .await
        .assert_status(401)
        .assert_body_contains(r#""code":"unauthorized""#);

    // The SPA index answers the named client routes and nothing else; an
    // unmatched GET that reaches it is a 404, not a scanner-probe `200`.
    server.get("/tree").await.assert_status(200);
    server.get("/nonexistent").await.assert_status(401);
    server.get("/tree/deeper").await.assert_status(401);
    // A `/sessions/…` path is a host's, and this bind is not one — but only
    // `GET /sessions` reaches the router here, because that is the probe the web
    // client tells the two bindings apart with (`web/src/session/entry.ts`). It
    // answers `404`, which reads as "not a host"; every other spelling of the
    // prefix is behind the credential check like anything else.
    server.get("/sessions").await.assert_status(404);
    server.get("/sessions/work").await.assert_status(401);
    server.get("/sessions/work/tree").await.assert_status(401);

    // A non-GET outside `/v1` fails closed at the gate. JSON-typed, to pass the
    // content-type check ahead of it, but bodiless: the gate answers without reading
    // one, and a body still being written races the in-memory connection's close.
    server
        .post("/nonexistent")
        .with_request_header(KnownHeaderName::ContentType, "application/json")
        .await
        .assert_status(401);
}

/// The gate's price list as a table (docs/web-bind.md). The rule is the
/// anonymous list inverted, so this is where a route's cost is settled: a path
/// `Surface::anonymous` does not name costs a device token, which is what keeps a
/// route added later from being readable to anyone who can reach the bind.
#[test]
fn the_anonymous_surface_is_named() {
    let anonymous = true;
    let credential = false;
    let cases = [
        // A session bind: the shell at its own base, the build's files, the
        // mint, and the probe the client reads a `404` from.
        (Surface::Session, Method::Get, "/", anonymous),
        (Surface::Session, Method::Get, "/tree", anonymous),
        (
            Surface::Session,
            Method::Get,
            "/assets/index-a1b2c3.js",
            anonymous,
        ),
        (Surface::Session, Method::Get, "/fonts/x.woff2", anonymous),
        (Surface::Session, Method::Get, "/icon-192.png", anonymous),
        (
            Surface::Session,
            Method::Get,
            "/apple-touch-icon.png",
            anonymous,
        ),
        (
            Surface::Session,
            Method::Get,
            "/splash/after-dark-1206x2622.png",
            anonymous,
        ),
        // The worker script, which the browser fetches to register the worker.
        (Surface::Session, Method::Get, "/sw.js", anonymous),
        (Surface::Session, Method::Post, "/auth/session", anonymous),
        (Surface::Session, Method::Get, "/sessions", anonymous),
        // A session bind's own surfaces, and everything else.
        (Surface::Session, Method::Get, "/v1/state", credential),
        (Surface::Session, Method::Get, "/files/notes.md", credential),
        (Surface::Session, Method::Get, "/inference", credential),
        (Surface::Session, Method::Get, "/nonesuch", credential),
        (Surface::Session, Method::Post, "/sessions", credential),
        // A host: the same shell, and the client's routes under a session
        // prefix — inside the tree whose every other path takes a credential.
        (Surface::Host, Method::Get, "/", anonymous),
        (Surface::Host, Method::Get, "/sessions/work", anonymous),
        (Surface::Host, Method::Get, "/sessions/work/tree", anonymous),
        (
            Surface::Host,
            Method::Get,
            "/assets/index-a1b2c3.js",
            anonymous,
        ),
        (Surface::Host, Method::Post, "/auth/session", anonymous),
        (Surface::Host, Method::Get, "/sessions", credential),
        (
            Surface::Host,
            Method::Get,
            "/sessions/work/v1/state",
            credential,
        ),
        // A host mounts `/files` under each session's prefix, priced like the
        // control paths beside it. A top-level `/files` is no host route.
        (
            Surface::Host,
            Method::Get,
            "/sessions/work/files",
            credential,
        ),
        (
            Surface::Host,
            Method::Get,
            "/sessions/work/files/notes.md",
            credential,
        ),
        (
            Surface::Host,
            Method::Get,
            "/sessions/work/filesystem",
            credential,
        ),
        (Surface::Host, Method::Get, "/files/notes.md", credential),
        // The host's own routes were the reason the rule was inverted: they sit
        // outside `/sessions` and were readable without a credential.
        (Surface::Host, Method::Get, "/inference", credential),
        (Surface::Host, Method::Get, "/inference/catalog", credential),
        // And a top-level route nothing has named yet is credential too.
        (Surface::Host, Method::Get, "/nonesuch", credential),
        (
            Surface::Session,
            Method::Get,
            "/nonesuch/deeper",
            credential,
        ),
    ];
    for (surface, method, path, free) in cases {
        assert_eq!(
            surface.anonymous(method, path),
            free,
            "{} {method:?} {path:?}",
            match surface {
                Surface::Session => "session",
                Surface::Host => "host",
            },
        );
    }
}

/// `/files` is the operator's read surface over the working directory
/// (docs/web-bind.md), behind a device token as `/v1` is: anonymous is refused
/// there unlike every other GET outside `/v1`, and a token opens it. Its
/// answers revalidate rather than being reused from a store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_takes_a_token() {
    let auth = armed("files-token");
    let (server, _guard) = tcp_server(&auth).await;

    server
        .get("/files/")
        .await
        .assert_status(401)
        .assert_body_contains(r#""code":"unauthorized""#);

    let listing = server
        .get("/files/")
        .with_request_header(KnownHeaderName::Authorization, enrolled(&auth))
        .await;
    listing.assert_status(200);
    assert_eq!(
        listing
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("private, no-cache")
    );
}

/// A device token opens `/files` on both surfaces through the gate itself: a
/// session bind's `/files` and a host's `/sessions/<id>/files` take it as `/v1` and
/// the session-prefixed control paths do, and refuse a conn without it. The routes
/// behind the gate are a stub; only the gate's answer is under test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_takes_a_token_on_both_surfaces() {
    let auth = armed("files-both-surfaces");
    let bearer = enrolled(&auth);
    for (surface, files, open) in [
        (Surface::Session, "/files/notes.md", "/v1/state"),
        (
            Surface::Host,
            "/sessions/work/files/notes.md",
            "/sessions/work/v1/state",
        ),
        (Surface::Host, "/sessions/work/files", "/sessions"),
    ] {
        let server = TestServer::new((
            gate(Arc::clone(&auth), surface),
            |conn: trillium::Conn| async move { conn.ok("reached") },
        ))
        .await
        .with_host("localhost");
        for path in [open, files] {
            server
                .get(path)
                .with_request_header(KnownHeaderName::Authorization, bearer.clone())
                .await
                .assert_status(200);
            server
                .get(path)
                .await
                .assert_status(401)
                .assert_body_contains(r#""code":"unauthorized""#);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsafe_methods_enforce_origin_and_content_type() {
    let auth = armed("csrf-hardening");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = enrolled(&auth);

    // A cross-site Origin is rejected even with a valid credential. Bodiless for the
    // same reason as the gate's 401 above: the rejection answers unread.
    server
        .post("/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::Origin, "https://evil.example")
        .await
        .assert_status(403)
        .assert_body_contains(r#""code":"forbidden""#);

    // A same-origin browser request passes (any port: the allowlist names hosts,
    // so a proxy in front on its own port passes too).
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
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(
            KnownHeaderName::ContentType,
            "application/x-www-form-urlencoded",
        )
        .with_body("text=hi")
        .await
        .assert_status(400);
    // `text/plain` is the other safelisted type a cross-site fetch can send unpreflighted.
    server
        .post("/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::ContentType, "text/plain")
        .with_body("{\"text\":\"hi\"}")
        .await
        .assert_status(400);
    // Raw bytes preflight cross-site, so an upload passes the gate.
    server
        .post("/v1/attachments?name=a.txt")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .with_request_header(KnownHeaderName::ContentType, "application/octet-stream")
        .with_body("hello")
        .await
        .assert_status(404)
        .assert_body_contains("no attachment store");
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

/// An h2 conn carries its authority in the `:authority` pseudo-header and no Host
/// header at all — the shape every browser produces against the TLS bind, since the
/// acceptor's ALPN negotiates h2. The gate must read it (`Conn::host()` alone does
/// not, trillium-http 1.6). Exercised at the handler level: `TestConn` speaks h1, so
/// the pseudo-header is set directly on the inner conn.
#[test]
fn an_h2_authority_satisfies_the_host_check() {
    let auth = armed("h2-authority");
    let gate = gate(Arc::clone(&auth), Surface::Session);

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

/// The `dev-proxy` build's module graph is anonymous, or the shell cannot load
/// the login view. Vite's virtual modules share a bare `/@` prefix, which a
/// directory match (`/@/…`) would miss.
#[cfg(feature = "dev-proxy")]
#[test]
fn the_dev_servers_module_graph_is_anonymous() {
    for path in [
        "/@vite/client",
        "/@react-refresh",
        "/@fs/Users/x/web/src/main.tsx",
        "/src/main.tsx",
        "/node_modules/.vite/deps/react.js",
    ] {
        for surface in [Surface::Session, Surface::Host] {
            assert!(surface.anonymous(Method::Get, path), "{path}");
        }
    }
    assert!(
        !Surface::Host.anonymous(Method::Get, "/srcery"),
        "a directory prefix is not a string prefix"
    );
}

/// Ids are validated before they are joined onto a path: only a plain segment names
/// a session, and the session-prefixed client routes are offered for exactly those.
#[test]
fn only_plain_ids_name_a_session() {
    assert!(plain_id("work-1.2"));
    for bad in ["", ".", "..", "a/b", "/etc", "a b", "wörk"] {
        assert!(!plain_id(bad), "{bad:?} passed as a session id");
    }
    assert_eq!(session_scoped("/sessions/work"), Some("/"));
    assert_eq!(session_scoped("/sessions/work/"), Some("/"));
    assert_eq!(session_scoped("/sessions/work/tree"), Some("/tree"));
    assert_eq!(session_scoped("/sessions/../tree"), None);
    assert_eq!(session_scoped("/v1/state"), None);
}
