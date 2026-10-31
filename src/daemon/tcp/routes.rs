//! The routes mounted beside the control API (docs/web-bind.md): the sign-in routes a
//! web transport carries — a browser asking to sign in, and collecting its token — the
//! enrollment routes only the socket carries — the sign-in link, approval, and the
//! enrolled devices — and the web client handler a web transport mounts last
//! (docs/web-client.md).

use super::gate::Surface;
use super::*;
use crate::daemon::views::answer;
use crate::devices::DeviceId;
use crate::protocol::answers::{StatusBody, StatusWord};
use crate::protocol::channel_value;
use crate::protocol::sign_in::{
    DeviceList, PendingSignInList, RemovedDevice, SignInClaim, SignInLink, SignInRequest,
};
use crate::sign_ins::{Claim, ClaimSecret, ShortCode, TTL_SECS};
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

/// The browser-facing anonymous surface of a web transport: a browser asking to sign
/// in, and collecting the token once the host approves. A page with no credential
/// cannot present one, so the gate names both as free (docs/web-bind.md). Mounted only
/// by [`handler`](super::super::handler::handler).
pub(crate) fn pair_routes(auth: Arc<TcpAuth>) -> Router {
    Router::new()
        .post("/pair", with_auth(Some(Arc::clone(&auth)), request_sign_in))
        .post("/pair/claim", with_auth(Some(auth), claim_sign_in))
}

/// Record a browser asking to sign in, from `{"label": …}` (cleaned; `browser` when
/// absent), and answer the page alone with `{"claim", "code", "expires_in"}`: the claim
/// secret it polls `POST /pair/claim` with, the short code it displays for the operator
/// to type into `efference devices approve`, and the seconds both live.
async fn request_sign_in(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
    #[derive(Deserialize)]
    struct RequestBody {
        /// What the page calls the browser, such as "Safari on macOS". Display text.
        #[serde(default)]
        label: Option<String>,
    }
    let body = conn.request_body_string().await.unwrap_or_default();
    let body = match serde_json::from_str::<RequestBody>(&body) {
        Ok(body) => body,
        Err(e) => return fail(conn, ErrorCode::BadRequest, format!("bad /pair body: {e}")),
    };
    let label = body.label.as_deref().unwrap_or_default();
    match auth.sign_ins.request(label, SystemTime::now()) {
        Ok(requested) => credential_answer(
            conn,
            200,
            SignInRequest {
                claim: requested.claim.encoded(),
                code: requested.code.to_string(),
                expires_in: TTL_SECS,
            },
        ),
        Err(e) => {
            log::error!("could not record a sign-in request: {e}");
            fail(conn, ErrorCode::Internal, "could not record the sign-in")
        }
    }
}

/// Collect a sign-in with `{"claim": …}`: `202 {"status":"pending"}` while it waits for
/// approval; `200 {"token": …}` on the first claim after, which enrolls a `browser`
/// device and spends the claim; `404` once nothing answers to it. The page may send its
/// `label` too, which names the device when the sign-in is a link's: a link is minted
/// before any page asks, while an approved request keeps the label the operator saw
/// when approving it.
async fn claim_sign_in(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
    #[derive(Deserialize)]
    struct ClaimBody {
        claim: String,
        #[serde(default)]
        label: Option<String>,
    }
    let body = conn.request_body_string().await.unwrap_or_default();
    let body = match serde_json::from_str::<ClaimBody>(&body) {
        Ok(body) => body,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /pair/claim body: {e}"),
            );
        }
    };
    let Some(claim) = ClaimSecret::parse(body.claim.trim()) else {
        return fail(conn, ErrorCode::NotFound, UNKNOWN_CLAIM);
    };
    match auth.sign_ins.claim(&claim, SystemTime::now()) {
        Ok(Claim::Waiting) => credential_answer(
            conn,
            202,
            SignInClaim::Waiting(StatusBody::new(StatusWord::Pending)),
        ),
        Ok(Claim::Approved { label, joined }) => {
            let label = match joined {
                Joined::Link => body.label.as_deref().unwrap_or(&label),
                Joined::Code | Joined::Add => &label,
            };
            match auth.enroll(Kind::Browser, label, joined) {
                Ok(enrolled) => credential_answer(
                    conn,
                    200,
                    SignInClaim::Approved {
                        token: enrolled.token,
                    },
                ),
                // The claim is spent either way: its row went before the insert, and a
                // claim that could enroll twice is the replay the delete exists to stop.
                Err(e) => {
                    log::error!("could not record a signed-in browser: {e}");
                    fail(conn, ErrorCode::Internal, "could not record the device")
                }
            }
        }
        Ok(Claim::Unknown) => fail(conn, ErrorCode::NotFound, UNKNOWN_CLAIM),
        Err(e) => {
            log::error!("could not read the pending sign-ins: {e}");
            fail(
                conn,
                ErrorCode::Internal,
                "could not read the pending sign-ins",
            )
        }
    }
}

/// What a claim that names nothing is told. One sentence for every case, so a claim's
/// answer does not say which one it was.
const UNKNOWN_CLAIM: &str = "no pending sign-in answers to that claim: it was claimed \
    already, it expired, or newer requests displaced it; ask for a new code";

/// An answer carrying a credential, which is never stored for reuse — the
/// token-endpoint rule (RFC 6749 §5.1). `/auth` and `/pair` sit outside the `/v1`
/// prefix that `SecurityHeaders` covers, so each such answer says so itself.
fn credential_answer(conn: Conn, status: u16, body: impl Serialize) -> Conn {
    conn.with_status(status)
        .with_response_header(KnownHeaderName::ContentType, "application/json")
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .with_body(channel_value(body).to_string())
        .halt()
}

/// The enrollment surface, mounted only on a socket — possession of the host machine
/// is the enrollment authority (docs/web-bind.md). A link and an approval each let a
/// browser collect a token, and adding a device mints one outright, so none rides a TCP
/// listener, authenticated or not. With no bind armed the routes exist but answer 404,
/// so the CLI verb gets a sentence instead of a route mismatch. Mounted only by
/// [`handler`](super::super::handler::handler).
pub(crate) fn enrollment_routes(auth: Option<Arc<TcpAuth>>) -> Router {
    Router::new()
        .get("/auth/link", with_auth(auth.clone(), auth_link))
        .get("/auth/pending", with_auth(auth.clone(), list_pending))
        .post("/auth/approve", with_auth(auth.clone(), approve_sign_in))
        .get("/auth/devices", with_auth(auth.clone(), list_devices))
        .post("/auth/devices", with_auth(auth.clone(), add_device))
        .delete("/auth/devices/:id", with_auth(auth, remove_device))
}

/// The sign-in link, `{"url": …}`: a pending sign-in approved before it was asked for,
/// its claim secret in the fragment, so the answer is `no-store` like any that carries
/// a credential.
async fn auth_link(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    match auth.mint_link() {
        Ok(url) => credential_answer(conn, 200, SignInLink { url }),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not record the sign-in: {e}"),
        ),
    }
}

/// The sign-ins waiting for approval, oldest first, as `{"pending": [{"label",
/// "created_at"}]}`. Never a code: approval is typing what the browser shows, never
/// choosing from a list, which would let a hurried operator pick another browser's row.
async fn list_pending(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    match auth.sign_ins.waiting(SystemTime::now()) {
        Ok(pending) => answer(conn, 200, PendingSignInList { pending }),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not read the pending sign-ins: {e}"),
        ),
    }
}

/// Approve the waiting sign-in a short code names, from `{"code": …}` as the operator
/// typed it — either case, the dash optional. Answers what was approved, `{"label",
/// "created_at"}`, so the operator sees what they let in; `404` when no live sign-in
/// waits under the code.
async fn approve_sign_in(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
    #[derive(Deserialize)]
    struct ApproveBody {
        code: String,
    }
    let body = conn.request_body_string().await.unwrap_or_default();
    let typed = match serde_json::from_str::<ApproveBody>(&body) {
        Ok(body) => body.code,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /auth/approve body: {e}"),
            );
        }
    };
    let Some(code) = ShortCode::parse(&typed) else {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "a short code is six letters and digits, as the browser shows it, like KQ7-M4X",
        );
    };
    match auth.sign_ins.approve(&code, SystemTime::now()) {
        Ok(Some(pending)) => answer(conn, 200, pending),
        Ok(None) => fail(
            conn,
            ErrorCode::NotFound,
            format!(
                "no sign-in is waiting under that code: a code is approved once, within \
                 {TTL_SECS} s of the browser showing it"
            ),
        ),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not read the pending sign-ins: {e}"),
        ),
    }
}

/// The enrolled devices, oldest first, as `{"devices": […]}`.
async fn list_devices(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    match auth.devices.list() {
        Ok(devices) => answer(conn, 200, DeviceList { devices }),
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
        Ok(enrolled) => credential_answer(conn, 201, enrolled),
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
        Ok(true) => answer(conn, 200, RemovedDevice { removed: id }),
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

/// [`Surface::spa_route`] as the fn pointer [`frontend()`] takes, one per surface.
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
