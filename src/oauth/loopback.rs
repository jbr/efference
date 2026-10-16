//! The loopback redirect listener (docs/mcp-auth.md).
//!
//! The authorization response arrives as a browser navigation to
//! `http://127.0.0.1:<port>/callback`, not as anything this process asked for.
//! So a server has to be listening before the browser opens, and it has to
//! stay up exactly long enough to catch one request.
//!
//! ## Why the port is fixed
//!
//! The usual native-app pattern is an ephemeral port (RFC 8252 §7.3 tells
//! authorization servers to allow the port to vary on a loopback redirect).
//! MCP overrides that: authorization servers **MUST** validate redirect URIs
//! by exact match against pre-registered values. A varying port cannot be
//! pre-registered — not in a static Client ID Metadata Document, and not in a
//! dynamic registration whose whole value is being reusable next time.
//!
//! So [`listen`] walks a small ladder of fixed ports instead, and every rung
//! is declared in the client metadata. The ladder exists so that one occupied
//! port is not a dead end; it is not a fallback to an arbitrary port.
//!
//! ## Both host spellings
//!
//! `127.0.0.1` and `localhost` name the same listener but are different
//! strings, and authorization servers differ on which they will accept — some
//! reject `localhost` because it can resolve to something else. The redirect
//! URI this module builds uses `127.0.0.1` (the literal, always
//! unambiguous); the `localhost` spelling of each port is registered too, so a
//! server that insists on it has something to match.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;
use trillium::{Conn, Method, Status};
use trillium_client::url::form_urlencoded;
use trillium_tokio::Swansong;

/// The path the redirect URI points at for a dynamically registered client,
/// which declares its own. Fixed rather than random: it is part of the
/// registered redirect URI, so it cannot vary per request.
///
/// An issuer that registered the client itself dictates the path instead —
/// `auth.openai.com` expects `/auth/callback` — so [`listen`] takes it rather
/// than assuming this one.
pub const CALLBACK_PATH: &str = "/callback";

/// How long to wait for the operator to finish authorizing in the browser
/// before giving up. Generous — it spans reading a consent screen, and
/// possibly a fresh login with a second factor.
pub const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(300);

/// The decoded query of an authorization response.
type Params = Vec<(String, String)>;

/// The one-shot delivery slot, shared with the handler. `Option` because the
/// send consumes it and only the first callback may deliver; `Mutex` because a
/// trillium handler is shared across connections.
type Delivery = Arc<Mutex<Option<oneshot::Sender<Params>>>>;

/// A bound listener waiting for one authorization response.
pub struct Loopback {
    port: u16,
    /// The callback path this listener answers on, so the redirect URI it hands
    /// out and the route it serves cannot disagree.
    path: &'static str,
    swansong: Swansong,
    received: oneshot::Receiver<Params>,
}

/// Bind the first available port in `ports`.
///
/// Every rung is tried before failing, and the error names all of them —
/// "port 50505 is in use" is not actionable on its own when the caller only
/// knows about a ladder.
pub async fn listen(ports: &[u16]) -> anyhow::Result<Loopback> {
    listen_at(ports, CALLBACK_PATH).await
}

/// [`listen`] at a callback path the issuer dictates.
pub async fn listen_at(ports: &[u16], path: &'static str) -> anyhow::Result<Loopback> {
    let (sender, received) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let swansong = Swansong::new();

    let mut failures = Vec::new();
    for &port in ports {
        let config = trillium_tokio::config()
            // The daemon owns Ctrl-C (docs/process-monitor.md). A server that
            // installed its own signal handler would race it, and this one
            // outlives its usefulness by seconds.
            .without_signals()
            .with_swansong(swansong.clone())
            .listeners()
            .bind_tcp(("127.0.0.1", port));
        match config {
            Ok(listeners) => {
                // The returned `ServerHandle` is not needed: the port is known
                // (we chose it) and shutdown goes through the `Swansong` we
                // already hold. Dropping the handle does not stop the server.
                listeners.spawn(callback_handler(sender.clone(), path));
                return Ok(Loopback {
                    port,
                    path,
                    swansong,
                    received,
                });
            }
            Err(e) => failures.push(format!("{port} ({e})")),
        }
    }
    anyhow::bail!(
        "could not bind a loopback redirect listener; every port in the ladder was unavailable: {}",
        failures.join(", ")
    )
}

impl Loopback {
    /// The `redirect_uri` to send on the authorization and token requests.
    /// Both must carry byte-identical values, so both read it from here.
    pub fn redirect_uri(&self) -> String {
        self.redirect_uri_for("127.0.0.1")
    }

    /// The same, spelled with the host the issuer registered. The listener
    /// binds `127.0.0.1` either way; the spelling is what the issuer
    /// exact-matches, and `auth.openai.com` registered `localhost`.
    pub fn redirect_uri_for(&self, host: &str) -> String {
        format!("http://{host}:{}{}", self.port, self.path)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Wait for the browser to arrive, and hand back the decoded query.
    ///
    /// Consumes the listener: exactly one authorization response is expected,
    /// and leaving a loopback port open after it has served its purpose is a
    /// standing invitation to feed this process an unsolicited code.
    pub async fn wait(self) -> anyhow::Result<Params> {
        let received = tokio::time::timeout(AUTHORIZATION_TIMEOUT, self.received).await;
        // Shut down before interpreting the result, so an error path closes
        // the port too.
        self.swansong.shut_down();
        match received {
            Ok(Ok(params)) => Ok(params),
            Ok(Err(_)) => anyhow::bail!("the loopback listener stopped before a response arrived"),
            Err(_) => anyhow::bail!(
                "no authorization response arrived within {}s",
                AUTHORIZATION_TIMEOUT.as_secs()
            ),
        }
    }
}

/// A redirect URI for one host spelling and port.
pub fn redirect_uri(host: &str, port: u16) -> String {
    format!("http://{host}:{port}{CALLBACK_PATH}")
}

/// Every redirect URI to declare in client metadata: both host spellings of
/// every rung in the ladder, because any of them may end up being the one used
/// and the authorization server matches exactly.
pub fn all_redirect_uris(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .flat_map(|&port| ["127.0.0.1", "localhost"].map(|host| redirect_uri(host, port)))
        .collect()
}

/// The handler: capture the query of the first `GET /callback`, answer the
/// browser, and ignore everything else.
fn callback_handler(sender: Delivery, path: &'static str) -> impl trillium::Handler {
    move |conn: Conn| {
        let sender = sender.clone();
        async move {
            if conn.method() != Method::Get || conn.path() != path {
                return conn.with_status(Status::NotFound).halt();
            }
            let params = decode_query(conn.querystring());
            // `take` makes this once-only: a second navigation to the callback
            // (a refresh, a stale tab) must not overwrite a response already
            // being validated.
            let first = sender.lock().unwrap().take();
            match first {
                Some(sender) => {
                    let _ = sender.send(params);
                    conn.with_status(Status::Ok)
                        .with_response_header("content-type", "text/html; charset=utf-8")
                        .with_body(RESPONSE_PAGE)
                        .halt()
                }
                None => conn
                    .with_status(Status::Ok)
                    .with_response_header("content-type", "text/html; charset=utf-8")
                    .with_body(ALREADY_RECEIVED_PAGE)
                    .halt(),
            }
        }
    }
}

/// Decode a redirect's query string into pairs.
fn decode_query(querystring: &str) -> Params {
    form_urlencoded::parse(querystring.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Served locally rather than by redirecting somewhere — there is an HTTP
/// server bound at this moment, and sending the browser (which is holding an
/// authorization code in its URL bar) to a remote page would be gratuitous.
const RESPONSE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>efference</title>\
<body style=\"font-family:system-ui;max-width:32em;margin:6em auto\">\
<h1>Authorized</h1><p>efference has the token it needed. You can close this tab.</p>";

const ALREADY_RECEIVED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>efference</title>\
<body style=\"font-family:system-ui;max-width:32em;margin:6em auto\">\
<h1>Already received</h1><p>This authorization was already completed. You can close this tab.</p>";

#[cfg(test)]
mod test;
