//! The routes mounted beside the control API (docs/web-bind.md): the credential mint a
//! web transport carries, the enrollment routes only the socket carries — the
//! bootstrap link, the authenticator URI, and the enrolled devices — and the web
//! client handler a web transport mounts last (docs/web-client.md).

use super::gate::Surface;
use super::*;
use crate::devices::DeviceId;
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
                    "no TCP bind armed — start the host with --bind",
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

/// Trade a current TOTP code for a browser device's token, answered in the body as
/// `{"token": …}` and never as a cookie. Rate-limited, and every accepted code burns;
/// an accepted code enrolls a `browser` device labelled from the body's optional
/// `label` (cleaned, `browser` when absent). The login view that reads `#token=` from
/// the fragment, POSTs here, and keeps what comes back is the web client's
/// (web/src/ui/Login.tsx, docs/web-bind.md).
async fn mint_session(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
    #[derive(Deserialize)]
    struct SessionBody {
        code: String,
        /// What the page calls the browser, such as "Safari on macOS". Display text.
        #[serde(default)]
        label: Option<String>,
    }
    let body = conn.request_body_string().await.unwrap_or_default();
    let body = match serde_json::from_str::<SessionBody>(&body) {
        Ok(body) => body,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /auth/session body: {e}"),
            );
        }
    };
    match auth.authenticate_code(body.code.trim()) {
        Ok(()) => match auth.enroll(
            Kind::Browser,
            body.label.as_deref().unwrap_or_default(),
            Joined::Code,
        ) {
            Ok(enrolled) => {
                credential_answer(conn, 200, serde_json::json!({ "token": enrolled.token }))
            }
            // The code is spent either way: it burned before the insert, and a code
            // that could mint twice would be the replay the burn exists to stop.
            Err(e) => {
                log::error!("could not record a signed-in browser: {e}");
                fail(conn, ErrorCode::Internal, "could not record the device")
            }
        },
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

/// An answer carrying a credential, which is never stored for reuse — the
/// token-endpoint rule (RFC 6749 §5.1). `/auth` sits outside the `/v1` prefix that
/// `SecurityHeaders` covers, so each such answer says so itself.
fn credential_answer(conn: Conn, status: u16, body: serde_json::Value) -> Conn {
    conn.with_status(status)
        .with_response_header(KnownHeaderName::ContentType, "application/json")
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .with_body(body.to_string())
        .halt()
}

/// The enrollment surface, mounted only on a session's socket — possession of the host
/// machine is the enrollment authority (docs/web-bind.md). The `otpauth` URI in
/// particular carries the TOTP secret itself, which mints codes at will, and adding a
/// device mints a token outright, so neither rides a TCP listener, authenticated or
/// not. With no bind armed the routes exist but answer 404, so the CLI verb gets a
/// sentence instead of a route mismatch. Mounted only by
/// [`handler`](super::super::handler::handler).
pub(crate) fn enrollment_routes(auth: Option<Arc<TcpAuth>>) -> Router {
    Router::new()
        .get("/auth/link", with_auth(auth.clone(), auth_link))
        .get("/auth/otpauth", with_auth(auth.clone(), auth_otpauth))
        .get("/auth/devices", with_auth(auth.clone(), list_devices))
        .post("/auth/devices", with_auth(auth.clone(), add_device))
        .delete("/auth/devices/:id", with_auth(auth, remove_device))
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

/// The enrolled devices, oldest first, as `{"devices": […]}`.
async fn list_devices(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    match auth.devices.list() {
        Ok(devices) => conn
            .with_status(200)
            .with_body(serde_json::json!({ "devices": devices }).to_string())
            .halt(),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not read the enrolled devices: {e}"),
        ),
    }
}

/// Enroll a script from `{"label": …}`, answering `{"device": …, "token": …}` — the
/// one answer that carries the token; no listing does.
async fn add_device(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
    #[derive(Deserialize)]
    struct AddBody {
        label: String,
    }
    let body = conn.request_body_string().await.unwrap_or_default();
    let label = match serde_json::from_str::<AddBody>(&body) {
        Ok(body) => body.label,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /auth/devices body: {e}"),
            );
        }
    };
    match auth.enroll(Kind::Script, &label, Joined::Add) {
        Ok(enrolled) => credential_answer(
            conn,
            201,
            serde_json::to_value(enrolled).unwrap_or_default(),
        ),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not record the device: {e}"),
        ),
    }
}

/// Revoke one device by its whole id. Its token is refused from the next request on,
/// by every daemon that reads the table.
async fn remove_device(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    let Some(id) = conn.param("id").and_then(DeviceId::parse_hex) else {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "a device is named by its whole id, 32 hex digits",
        );
    };
    match auth.devices.remove(&id) {
        Ok(true) => conn
            .with_status(200)
            .with_body(serde_json::json!({ "removed": id }).to_string())
            .halt(),
        Ok(false) => fail(
            conn,
            ErrorCode::NotFound,
            format!("no enrolled device {id}"),
        ),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not revoke the device: {e}"),
        ),
    }
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
