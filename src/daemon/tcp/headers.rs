//! The headers every listener stamps on its responses (docs/web-bind.md,
//! docs/web-client.md): the content-security policy the web client is served under, and
//! the transport-side cache and TLS headers.

use super::gate::{session_scoped, under};
use trillium::KnownHeaderName;

/// The policy the embedded build is served under. Everything the page loads is
/// same-origin and external — one module script and one stylesheet, both hashed,
/// with the grammar chunks imported dynamically from the same origin — so `'self'`
/// needs no hash, nonce, or `unsafe-` escape. Syntax highlighting needs no
/// `style-src` accommodation: token colours are set through React's `style` prop,
/// which is CSSOM, and CSP governs inline `<style>` elements and `style=""`
/// attributes parsed from markup rather than CSSOM mutations
/// (docs/web-client.md).
///
/// `img-src` adds `blob:` to `'self'`. The client draws the bytes it fetches from
/// the API, and the local preview of a file being attached, through object URLs,
/// which `'self'` does not match. `data:` is not named, so an inline image stays
/// blocked.
#[cfg(not(feature = "dev-proxy"))]
const CSP: &str = "default-src 'self'; img-src 'self' blob:; base-uri 'none'; \
                   frame-ancestors 'none'; form-action 'none'; object-src 'none'";

/// Under `dev-proxy` the page is Vite's, not the build's: it injects an inline
/// module preamble and talks to its HMR server over a WebSocket. Relaxing for that
/// means the strict policy above is not the one exercised in development — the
/// reason this is gated on the same `cfg` as the proxy rather than on a runtime
/// flag, so a release build cannot serve it. `img-src` is the shipped policy's,
/// so images drawn from object URLs load in development as they do in the build.
#[cfg(feature = "dev-proxy")]
const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
                   style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; \
                   img-src 'self' blob:; base-uri 'none'; frame-ancestors 'none'; \
                   object-src 'none'";

/// The transport-side cache and TLS headers, alongside the page-hardening ones:
/// `no-store` on every `/v1` route and HSTS on a TLS listener, described with
/// the bind (docs/web-bind.md).
pub(crate) struct SecurityHeaders {
    /// Whether the listener this stack serves terminates TLS, fixed at bind
    /// time like the rest of the stack rather than read off the conn.
    tls: bool,
}

impl SecurityHeaders {
    pub(crate) fn new(tls: bool) -> Self {
        Self { tls }
    }
}

impl trillium::Handler for SecurityHeaders {
    async fn before_send(&self, conn: trillium::Conn) -> trillium::Conn {
        // Control answers are never reused from a store: on a host the same
        // address names a different session's route, and the served shape
        // changes with the build (docs/control-api.md). A session-prefixed
        // path counts as its inner route, so a host's `/sessions/<id>/v1/…`
        // is covered too. `/files` sets its own `private, no-cache` in its
        // read stage, which is what keeps the header off its change streams.
        let route = session_scoped(conn.path()).unwrap_or(conn.path());
        let artifact = crate::daemon::artifacts::is_artifact_route(conn.path());
        let conn = if under(route, "/v1") {
            conn.with_response_header(KnownHeaderName::CacheControl, "no-store")
        } else {
            conn
        };
        // RFC 6797 defines the field for secure transports only, so a
        // plaintext loopback bind does not send it. No subdomain scope and no
        // preload: the bind is one host.
        let conn = if self.tls {
            conn.with_response_header(KnownHeaderName::StrictTransportSecurity, "max-age=31536000")
        } else {
            conn
        };
        // A cross-origin document ends up neither this page's opener nor its
        // openee: the page holds a session credential, and a cross-origin
        // window must not hold a reference into its browsing-context group.
        let (csp, frame_options) = if artifact {
            (crate::daemon::artifacts::ARTIFACT_CSP, "SAMEORIGIN")
        } else {
            (CSP, "DENY")
        };
        conn.with_response_header(KnownHeaderName::CrossOriginOpenerPolicy, "same-origin")
            .with_response_header(KnownHeaderName::ContentSecurityPolicy, csp)
            .with_response_header("x-content-type-options", "nosniff")
            // Redundant with `frame-ancestors` for anything current, and one
            // header for the browsers that predate CSP3.
            .with_response_header("x-frame-options", frame_options)
            .with_response_header(KnownHeaderName::ReferrerPolicy, "no-referrer")
    }
}

#[cfg(test)]
mod test;
