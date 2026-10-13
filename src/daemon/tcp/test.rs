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
/// `Surface::anonymous` does not name costs a credential, which is what keeps a
/// route added later from being readable to anyone who can reach the bind.
#[test]
fn the_anonymous_surface_is_named() {
    let anonymous = Zone::Anonymous;
    let credential = Zone::Credential;
    let cookie_only = Zone::CookieOnly;
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
        (
            Surface::Session,
            Method::Get,
            "/files/notes.md",
            cookie_only,
        ),
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
    for (surface, method, path, zone) in cases {
        assert_eq!(
            surface.zone(method, path),
            zone,
            "{} {method:?} {path:?}",
            match surface {
                Surface::Session => "session",
                Surface::Host => "host",
            },
        );
    }
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
    // A plaintext loopback bind: no `Secure`, which Safari refuses over http, and
    // so no `__Host-` prefix, which requires it.
    assert!(set_cookie.starts_with(auth::LOOPBACK_COOKIE_NAME));
    for attribute in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(
            set_cookie.contains(attribute),
            "{attribute} in {set_cookie}"
        );
    }
    assert!(!set_cookie.contains("Secure"), "{set_cookie}");

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

/// Browsers split the cookie header into one field line per cookie over h2/h3
/// (RFC 9113 §8.2.3), and cookies ignore ports — so any other service's cookie
/// on the daemon's host forces the split. The gate must find its cookie among
/// several field lines and among several crumbs on one line; the regressed
/// shape was a raw `get_str`, which reads none of a multi-valued header, so
/// browsers carrying any unrelated host cookie could never authenticate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cookie_authenticates_among_other_cookies_and_split_field_lines() {
    let auth = armed("cookie-field-lines");
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
    let cookie = set_cookie.split(';').next().unwrap().to_string();

    // Two field lines, ours second.
    server
        .get("/v1/state")
        .with_request_header(
            KnownHeaderName::Cookie,
            trillium::HeaderValues::from(vec!["unrelated=1".to_string(), cookie.clone()]),
        )
        .await
        .assert_status(200);

    // One line, several crumbs, ours last.
    server
        .get("/v1/state")
        .with_request_header(
            KnownHeaderName::Cookie,
            format!("unrelated=1; other=two; {cookie}"),
        )
        .await
        .assert_status(200);
}

/// `/files` is the operator's read surface over the working directory
/// (docs/web-bind.md), and the one route the bearer does not open. A bearer holder
/// reading it would bypass `[paths]` — including the deny that keeps a session out
/// of every other session's log — so the gate requires the browser's cookie there
/// and says why, with a 403 rather than a 401: the credential is valid, and
/// presenting it again would not help.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_takes_the_cookie_and_refuses_the_bearer() {
    let auth = armed("files-cookie-only");
    let (server, _guard) = tcp_server(&auth).await;

    // Anonymous is not enough, unlike every other GET outside `/v1`.
    server
        .get("/files/")
        .await
        .assert_status(401)
        .assert_body_contains(r#""code":"unauthorized""#);

    // The bearer opens `/v1` and not this.
    let bearer = format!("Bearer {}", auth.bearer());
    server
        .get("/v1/state")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .await
        .assert_status(200);
    server
        .get("/files/")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .await
        .assert_status(403)
        .assert_body_contains(r#""code":"forbidden""#);

    // The cookie does open it — the surface exists, it is only credential-scoped.
    let session = server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": current_code(&auth) }))
        .await;
    session.assert_status(200);
    let cookie = session
        .header(KnownHeaderName::SetCookie)
        .expect("a session cookie was set")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    server
        .get("/files/")
        .with_request_header(KnownHeaderName::Cookie, cookie)
        .await
        .assert_status(200);
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

    // A cross-site Origin is rejected even with a valid credential. Bodiless for the
    // same reason as the gate's 401 above: the rejection answers unread.
    server
        .post("/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_request_header(KnownHeaderName::Origin, "https://evil.example")
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
            assert_eq!(surface.zone(Method::Get, path), Zone::Anonymous, "{path}");
        }
    }
    assert_eq!(
        Surface::Host.zone(Method::Get, "/srcery"),
        Zone::Credential,
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

/// A TLS bind keeps the `__Host-` cookie, which requires `Secure`; only plaintext
/// loopback drops it (docs/web-bind.md).
#[test]
fn only_a_plaintext_bind_drops_the_secure_cookie() {
    let (tls, _listener) = arm_with_ledger(
        TcpBind {
            addr: "0.0.0.0:0".into(),
            tls: Some(TlsPem {
                cert: Vec::new(),
                key: Vec::new(),
            }),
            public_url: "https://host.example:8443".into(),
            root: Root::new("tls-cookie"),
            open: false,
        },
        scratch_ledger(),
    );
    assert_eq!(tls.cookie_name(), auth::COOKIE_NAME);
    assert_eq!(
        armed("plain-cookie").cookie_name(),
        auth::LOOPBACK_COOKIE_NAME
    );
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
