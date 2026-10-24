//! Tests for the TCP bind's gate and routes (docs/web-bind.md), over the same
//! in-memory transport the other daemon tests use — the full stack a TCP conn
//! traverses (gate → public router → `/v1` router), no socket bound.

use super::*;
use crate::daemon::{App, Transport, handler};
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
            public_url: "http://localhost:7777".into(),
            root: Root::new(root),
            open: false,
        },
        scratch_ledger(),
    );
    auth
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
async fn tcp_server(auth: &Arc<TcpAuth>) -> (TestServer<impl trillium::Handler>, TurnsGuard) {
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
struct TurnsGuard(#[allow(dead_code)] mpsc::UnboundedReceiver<SubmittedTurn>);

/// The six digits currently valid for this auth's derived TOTP secret.
fn current_code(auth: &TcpAuth) -> String {
    auth::format_code(auth::totp(
        &auth.totp_secret,
        auth::timestep(std::time::SystemTime::now()),
    ))
}

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
/// `Surface::anonymous` does not name costs the bearer, which is what keeps a
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

/// `/files` is the operator's read surface over the working directory
/// (docs/web-bind.md), behind the same bearer as `/v1`: anonymous is refused
/// there unlike every other GET outside `/v1`, and the bearer opens it. Its
/// answers revalidate rather than being reused from a store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_takes_the_bearer() {
    let auth = armed("files-bearer");
    let (server, _guard) = tcp_server(&auth).await;

    server
        .get("/files/")
        .await
        .assert_status(401)
        .assert_body_contains(r#""code":"unauthorized""#);

    let listing = server
        .get("/files/")
        .with_request_header(
            KnownHeaderName::Authorization,
            format!("Bearer {}", auth.bearer()),
        )
        .await;
    listing.assert_status(200);
    assert_eq!(
        listing
            .response_headers()
            .get_str(KnownHeaderName::CacheControl),
        Some("private, no-cache")
    );
}

/// The bearer opens `/files` on both surfaces through the gate itself: a session
/// bind's `/files` and a host's `/sessions/<id>/files` take it as `/v1` and the
/// session-prefixed control paths do, and refuse a conn without it. The routes
/// behind the gate are a stub; only the gate's answer is under test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_takes_the_bearer_on_both_surfaces() {
    let auth = armed("files-both-surfaces");
    let bearer = format!("Bearer {}", auth.bearer());
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

/// The bearer authenticates in the `Authorization` header and nowhere else. A
/// cookie and a query parameter are what a request another site forces could
/// carry — a navigation, a form, an image — so the same value there is refused
/// (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_authorization_header_carries_the_bearer() {
    let auth = armed("bearer-authenticates");
    let (server, _guard) = tcp_server(&auth).await;
    let token = auth.bearer();

    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, format!("Bearer {token}"))
        .await
        .assert_status(200);

    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, "Bearer not-the-bearer")
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
    let bearer = format!("Bearer {}", auth.bearer());
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
/// an answer nor send the header the bearer rides (docs/web-bind.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_answer_grants_another_origin() {
    let auth = armed("no-cors");
    let (server, _guard) = tcp_server(&auth).await;
    let bearer = format!("Bearer {}", auth.bearer());
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

/// Every response the bind produces carries the security headers, whatever
/// produced it — the SPA index, a hashed asset, and an unauthenticated `/v1`
/// rejection alike. `SecurityHeaders` sits outermost so its `before_send` runs
/// last; a handler that answered without it would be the hole
/// (docs/web-client.md).
#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let auth = armed("root");
    let (server, _turns) = tcp_server(&auth).await;
    for path in ["/", "/v1/state", "/nonexistent"] {
        let conn = server.get(path).await;
        let csp = conn
            .response_headers()
            .get_str(KnownHeaderName::ContentSecurityPolicy)
            .unwrap_or_default()
            .to_string();
        assert!(
            csp.contains("frame-ancestors 'none'") && csp.contains("object-src 'none'"),
            "{path} carried no usable content-security-policy: {csp:?}"
        );
        assert_eq!(
            conn.response_headers().get_str("x-content-type-options"),
            Some("nosniff"),
            "{path} did not deny content sniffing"
        );
        assert_eq!(
            conn.response_headers().get_str("x-frame-options"),
            Some("DENY"),
            "{path} did not deny framing"
        );
        assert_eq!(
            conn.response_headers()
                .get_str(KnownHeaderName::CrossOriginOpenerPolicy),
            Some("same-origin"),
            "{path} did not isolate its browsing context"
        );
        // Control answers are never reused from a store; the frontend and 404
        // answers set none (docs/web-bind.md).
        assert_eq!(
            conn.response_headers()
                .get_str(KnownHeaderName::CacheControl),
            if path.starts_with("/v1") {
                Some("no-store")
            } else {
                None
            },
            "{path} cache-control"
        );
        assert!(
            conn.response_headers()
                .get_str(KnownHeaderName::StrictTransportSecurity)
                .is_none(),
            "the plaintext loopback bind sends no HSTS"
        );
    }
}

/// HSTS rides only on a TLS listener (docs/web-bind.md): the layer's `tls`
/// flag comes from the arm, fixed at bind time rather than read off the conn.
#[tokio::test]
async fn hsts_rides_only_when_the_layer_is_tls() {
    for (tls, expected) in [(true, Some("max-age=31536000")), (false, None)] {
        let server = TestServer::new(SecurityHeaders::new(tls)).await;
        let conn = server.get("/").await;
        assert_eq!(
            conn.response_headers()
                .get_str(KnownHeaderName::StrictTransportSecurity),
            expected,
            "tls={tls}"
        );
    }
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

/// The embedded build loads nothing inline and nothing cross-origin, so its policy
/// admits no `unsafe-` escape. Pinned because relaxing it is exactly the change
/// that should have to be argued for, and because the `dev-proxy` policy — which
/// does relax, for Vite — must not be able to reach a release build.
#[cfg(not(feature = "dev-proxy"))]
#[tokio::test]
async fn the_shipped_policy_admits_no_unsafe_escape() {
    let auth = armed("root");
    let (server, _turns) = tcp_server(&auth).await;
    let conn = server.get("/").await;
    let csp = conn
        .response_headers()
        .get_str(KnownHeaderName::ContentSecurityPolicy)
        .unwrap_or_default()
        .to_string();
    assert!(csp.starts_with("default-src 'self'"), "{csp:?}");
    assert!(
        !csp.contains("unsafe-"),
        "shipped policy must stay strict: {csp:?}"
    );
}

/// The policy is the boundary for script that could reach the bearer
/// (docs/web-bind.md): every source it names is the page's own origin, `blob:`
/// for drawn images, or nothing — no other origin, scheme, or wildcard to
/// connect to, load from, or submit a form to.
#[cfg(not(feature = "dev-proxy"))]
#[tokio::test]
async fn the_shipped_policy_names_no_other_origin() {
    let auth = armed("root");
    let (server, _turns) = tcp_server(&auth).await;
    let conn = server.get("/").await;
    let csp = conn
        .response_headers()
        .get_str(KnownHeaderName::ContentSecurityPolicy)
        .unwrap_or_default()
        .to_string();
    for directive in csp.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let mut parts = directive.split_whitespace();
        let name = parts.next().unwrap_or_default();
        for source in parts {
            assert!(
                matches!(source, "'self'" | "'none'" | "blob:"),
                "{name} names {source} in {csp:?}"
            );
        }
    }
}

/// The client draws API bytes and local file previews from object URLs, which
/// `default-src 'self'` does not admit, so `img-src` names `blob:` — and only
/// that beyond `'self'`: an inline `data:` image stays blocked. Both the shipped
/// and the `dev-proxy` policy carry the directive (docs/web-client.md).
#[tokio::test]
async fn the_policy_admits_object_url_images_and_no_data_images() {
    let auth = armed("root");
    let (server, _turns) = tcp_server(&auth).await;
    let conn = server.get("/").await;
    let csp = conn
        .response_headers()
        .get_str(KnownHeaderName::ContentSecurityPolicy)
        .unwrap_or_default()
        .to_string();
    let img_src = csp
        .split(';')
        .map(str::trim)
        .find(|directive| directive.starts_with("img-src"));
    assert_eq!(img_src, Some("img-src 'self' blob:"), "{csp:?}");
    assert!(!csp.contains("data:"), "no data: source in {csp:?}");
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

/// The arm records whether its listener terminates TLS, which is what HSTS
/// reads (docs/web-bind.md); a plaintext loopback bind is not TLS.
#[test]
fn only_a_tls_arm_is_tls() {
    let (tls, _listener) = arm_with_ledger(
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
        scratch_ledger(),
    );
    assert!(tls.is_tls());
    assert!(!armed("plain-arm").is_tls());
}

/// A bind to port 0 mints links naming the port it actually bound; a configured
/// public URL with its own port is left as given.
#[test]
fn a_port_zero_bind_mints_links_on_its_bound_port() {
    let (auth, _listener) = arm_with_ledger(
        TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            public_url: "http://localhost:0".into(),
            root: Root::new("port-zero"),
            open: false,
        },
        scratch_ledger(),
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
