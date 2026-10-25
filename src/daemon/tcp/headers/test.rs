//! Tests for the response headers (docs/web-bind.md, docs/web-client.md). The helpers are
//! in `tcp/test.rs`.

use super::super::test::{armed, tcp_server};
use super::*;
use trillium_testing::TestServer;

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

#[tokio::test]
async fn artifact_routes_keep_the_artifact_policy_and_allow_same_origin_framing() {
    let document = format!("/artifacts/brisk-otter/12/0/3/{}", "A".repeat(43));
    let server = TestServer::new((
        SecurityHeaders::new(false),
        |conn: trillium::Conn| async move { conn.ok("artifact") },
    ))
    .await;

    for path in [
        document.clone(),
        "/artifacts/_efference/runtime.js".to_string(),
        "/artifacts/_efference/theme.css".to_string(),
    ] {
        let response = server.get(&path).await;
        let headers = response.response_headers();
        let csp = headers
            .get_str(KnownHeaderName::ContentSecurityPolicy)
            .unwrap_or_default();
        assert_eq!(csp, crate::daemon::artifacts::ARTIFACT_CSP, "{path}");
        assert!(!csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(headers.get_str("x-frame-options"), Some("SAMEORIGIN"));
        assert_eq!(headers.get_str("x-content-type-options"), Some("nosniff"));
        assert_eq!(
            headers.get_str(KnownHeaderName::ReferrerPolicy),
            Some("no-referrer")
        );
    }

    let prefixed = server
        .get(&format!("/sessions/brisk-otter{document}"))
        .await;
    assert_ne!(
        prefixed
            .response_headers()
            .get_str(KnownHeaderName::ContentSecurityPolicy),
        Some(crate::daemon::artifacts::ARTIFACT_CSP)
    );
    assert_eq!(
        prefixed.response_headers().get_str("x-frame-options"),
        Some("DENY")
    );
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

/// The policy is the boundary for script that could reach the page's token
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

/// A sandboxed artifact's origin is opaque, so its font loads are CORS requests
/// (docs/artifacts.md): the web client's fonts carry an allow-any-origin header, and
/// nothing else the shell serves does.
#[tokio::test]
async fn the_web_clients_fonts_allow_any_origin() {
    let server = TestServer::new((
        SecurityHeaders::new(false),
        |conn: trillium::Conn| async move { conn.ok("bytes") },
    ))
    .await;
    let allowed = |path: &'static str| {
        let server = &server;
        async move {
            server
                .get(path)
                .await
                .response_headers()
                .get_str(KnownHeaderName::AccessControlAllowOrigin)
                .map(str::to_owned)
        }
    };
    assert_eq!(
        allowed("/fonts/newsreader-var.woff2").await.as_deref(),
        Some("*")
    );
    assert_eq!(allowed("/assets/index.css").await, None);
    assert_eq!(allowed("/v1/state").await, None);
}
