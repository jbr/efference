//! The routes mounted beside the control API (docs/web-bind.md): the credential mint a
//! web transport carries, the enrollment routes only the socket carries, and the web
//! client handler a web transport mounts last (docs/web-client.md).

use super::gate::Surface;
use super::*;
use trillium::KnownHeaderName;

/// A route that needs the bind's credential state: `auth` is cloned into each conn's
/// call. `None` — a socket with no TCP bind armed — answers every such route with the
/// same sentence, so a CLI verb gets a reason rather than a route mismatch.
fn with_auth<F, Fut>(auth: Option<Arc<TcpAuth>>, route: F) -> impl trillium::Handler
where
    F: Fn(Arc<TcpAuth>, Conn) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Conn> + Send + 'static,
{
    move |conn: Conn| {
        let auth = auth.clone();
        async move {
            match auth {
                Some(auth) => route(auth, conn).await,
                None => fail(
                    conn,
                    ErrorCode::NotFound,
                    "no TCP bind armed — start serve with --bind",
                ),
            }
        }
    }
}

/// The browser-facing anonymous surface of a web transport: the credential mint.
/// Mounted only by [`handler`](super::super::handler::handler).
pub(crate) fn mint_routes(auth: Arc<TcpAuth>) -> Router {
    Router::new().post("/auth/session", with_auth(Some(auth), mint_session))
}

/// Trade a current TOTP code for the bearer, answered in the body as
/// `{"token": …}` and never as a cookie. The one credential mint; rate-limited,
/// and every accepted code burns. The login view that reads `#token=` from the
/// fragment, POSTs here, and keeps what comes back is the web client's
/// (web/src/ui/Login.tsx, docs/web-bind.md).
async fn mint_session(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
    #[derive(Deserialize)]
    struct SessionBody {
        code: String,
    }
    let body = conn.request_body_string().await.unwrap_or_default();
    let code = match serde_json::from_str::<SessionBody>(&body) {
        Ok(body) => body.code,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /auth/session body: {e}"),
            );
        }
    };
    match auth.authenticate_code(code.trim()) {
        // A credential in a body is never stored for reuse — the token-endpoint
        // rule (RFC 6749 §5.1), and `/auth` sits outside the `/v1` prefix that
        // `SecurityHeaders` covers.
        Ok(()) => conn
            .with_status(200)
            .with_response_header(KnownHeaderName::ContentType, "application/json")
            .with_response_header(KnownHeaderName::CacheControl, "no-store")
            .with_body(serde_json::json!({ "token": auth.bearer() }).to_string())
            .halt(),
        Err(CodeRejection::Locked(remaining)) => {
            let secs = remaining.as_secs().max(1);
            respond(
                conn.with_response_header(KnownHeaderName::RetryAfter, secs.to_string()),
                ErrorBody::new(
                    ErrorCode::RateLimited,
                    format!("verification locked; retry in {secs}s"),
                ),
            )
        }
        Err(CodeRejection::Wrong) => fail(conn, ErrorCode::Unauthorized, "code did not verify"),
        Err(CodeRejection::Replayed) => fail(
            conn,
            ErrorCode::Unauthorized,
            "code already used — mint a fresh link with `efference dev open`",
        ),
        Err(CodeRejection::Ledger) => {
            fail(conn, ErrorCode::Internal, "could not record the code burn")
        }
    }
}

/// The enrollment surface, mounted only on a session's socket — possession of the host
/// machine is the enrollment authority (docs/web-bind.md). The `otpauth` URI in
/// particular carries the TOTP secret itself, which mints codes at will, so it never
/// rides a TCP listener, authenticated or not. With no bind armed the routes exist but
/// answer 404, so the CLI verb gets a sentence instead of a route mismatch. Mounted
/// only by [`handler`](super::super::handler::handler).
pub(crate) fn enrollment_routes(auth: Option<Arc<TcpAuth>>) -> Router {
    Router::new()
        .get("/auth/link", with_auth(auth.clone(), auth_link))
        .get("/auth/otpauth", with_auth(auth.clone(), auth_otpauth))
        .get("/auth/bearer", with_auth(auth, auth_bearer))
}

/// The bootstrap link: the public base with a current code in the fragment.
async fn auth_link(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    match auth.mint_link() {
        Some(url) => conn
            .with_status(200)
            .with_body(serde_json::json!({ "url": url }).to_string())
            .halt(),
        None => fail(
            conn,
            ErrorCode::RateLimited,
            "every current code has been used; ask again in half a minute",
        ),
    }
}

/// The `otpauth://` URI an authenticator app enrolls from.
async fn auth_otpauth(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    conn.with_status(200)
        .with_body(serde_json::json!({ "uri": auth.otpauth_uri() }).to_string())
        .halt()
}

/// The bearer, for a program on this machine — the same value the mint hands a
/// browser.
async fn auth_bearer(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    conn.with_status(200)
        .with_body(serde_json::json!({ "bearer": auth.bearer() }).to_string())
        .halt()
}

/// [`Surface::spa_route`] as the fn pointer [`frontend`] takes, one per surface.
fn session_spa_route(conn: &Conn) -> bool {
    Surface::Session.spa_route(conn.path())
}

fn host_spa_route(conn: &Conn) -> bool {
    Surface::Host.spa_route(conn.path())
}

/// The web client (docs/web-client.md): `web/` built and embedded at compile time,
/// or — under the `dev-proxy` feature — proxied to a spawned Vite dev server with
/// HMR. Mounted last on a web transport, so it answers exactly the GETs the routes
/// don't claim; the gate has already let those through anonymously. A host
/// mounts the same build at its own root, which is why the index predicate is
/// surface-dependent: a host also answers the client's routes under a session
/// prefix, and the session bind does not.
pub(crate) fn frontend(surface: Surface) -> impl trillium::Handler {
    trillium_frontend::frontend!("./web")
        .with_client(crate::http::client())
        .with_index_file("index.html")
        // `trillium_frontend` takes a bare fn pointer, so the surface is chosen
        // between two fn items rather than captured in a closure.
        .with_index_predicate(match surface {
            Surface::Session => session_spa_route,
            Surface::Host => host_spa_route,
        })
}

#[cfg(test)]
mod test;
