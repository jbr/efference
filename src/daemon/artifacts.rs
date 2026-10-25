//! In-memory HTML artifacts resolved from committed assistant output.
//!
//! The authenticated resolver takes log coordinates; the token routes expose only the
//! selected HTML and the web-owned runtime resources. A separate loopback listener serves
//! the same token routes for clients that cannot load a same-origin URL (docs/control-api.md).

use std::collections::HashMap;
use std::io;
use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use trillium::{Conn, KnownHeaderName};
use trillium_router::Router;
use trillium_router::RouterConnExt;
use trillium_tokio::Swansong;

use crate::artifact::closed_html_artifact_fences;
use crate::log::Payload;

const TOKEN_BYTES: usize = 32;
const RUNTIME_JS: &str = include_str!("../../web/public/artifacts/runtime.js");
const THEME_CSS: &str = include_str!("../../web/public/artifacts/theme.css");

const RUNTIME_TAG: &str = "<script src=\"./_efference/runtime.js\"></script>";
pub(super) const ARTIFACT_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; \
    style-src 'self' 'unsafe-inline'; img-src data: blob:; font-src data:; \
    connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; \
    object-src 'none'; frame-ancestors 'self'; sandbox allow-scripts";

type HmacSha256 = Hmac<Sha256>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static LOOPBACK: tokio::sync::OnceCell<LoopbackServer> = tokio::sync::OnceCell::const_new();

/// The descriptor returned by the authenticated coordinate resolver.
#[derive(serde::Serialize)]
struct Resolution {
    url: String,
    external_url: String,
}

struct SessionIdentity {
    shared: Weak<dyn std::any::Any + Send + Sync>,
    nonce: [u8; TOKEN_BYTES],
}

struct Registry {
    secret: [u8; TOKEN_BYTES],
    sessions: Mutex<HashMap<usize, SessionIdentity>>,
    artifacts: Mutex<HashMap<String, String>>,
}

impl Registry {
    fn new() -> Self {
        let mut secret = [0; TOKEN_BYTES];
        rand::rng().fill_bytes(&mut secret);
        Self {
            secret,
            sessions: Mutex::default(),
            artifacts: Mutex::default(),
        }
    }

    fn session_nonce<T: std::any::Any + Send + Sync>(&self, shared: &Arc<T>) -> [u8; TOKEN_BYTES] {
        let erased: Arc<dyn std::any::Any + Send + Sync> = shared.clone();
        let key = Arc::as_ptr(&erased) as *const () as usize;
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(identity) = sessions.get(&key)
            && identity
                .shared
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, &erased))
        {
            return identity.nonce;
        }
        let mut nonce = [0; TOKEN_BYTES];
        rand::rng().fill_bytes(&mut nonce);
        sessions.retain(|_, identity| identity.shared.strong_count() > 0);
        sessions.insert(
            key,
            SessionIdentity {
                shared: Arc::downgrade(&erased),
                nonce,
            },
        );
        nonce
    }

    fn insert(
        &self,
        session_nonce: &[u8; TOKEN_BYTES],
        seq: u64,
        text_block: usize,
        fence: usize,
        html: String,
    ) -> String {
        let mut mac = <HmacSha256 as KeyInit>::new_from_slice(&self.secret)
            .expect("HMAC accepts a key of any length");
        mac.update(b"efference-html-artifact-v1\0");
        mac.update(session_nonce);
        mac.update(&seq.to_be_bytes());
        mac.update(&text_block.to_be_bytes());
        mac.update(&fence.to_be_bytes());
        let token = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        self.artifacts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.clone(), html);
        token
    }

    fn get(&self, token: &str) -> Option<String> {
        self.artifacts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(token)
            .cloned()
    }
}

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

/// Resolve one committed assistant text block and fenced artifact (`GET /v1/artifacts/:seq/:text_block/:fence`).
pub(crate) async fn resolve(conn: Conn) -> Conn {
    let Some(seq) = conn
        .param("seq")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return fail(conn, 400, "seq must be an unsigned integer");
    };
    let Some(text_block) = conn
        .param("text_block")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return fail(conn, 400, "text_block must be an unsigned integer");
    };
    let Some(fence_ordinal) = conn
        .param("fence")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return fail(conn, 400, "fence must be an unsigned integer");
    };
    let Some(session) = super::http::shared_of(&conn) else {
        return fail(conn, 500, "session state is unavailable");
    };
    let Some(entry) = session
        .snapshot()
        .into_iter()
        .find(|entry| entry.seq == seq)
    else {
        return fail(
            conn,
            404,
            "no committed assistant artifact at those coordinates",
        );
    };
    let Payload::Assistant {
        output,
        excluded: None,
    } = entry.payload
    else {
        return fail(
            conn,
            404,
            "no committed assistant artifact at those coordinates",
        );
    };
    let Some(text) = output.text().into_iter().nth(text_block) else {
        return fail(
            conn,
            404,
            "no committed assistant artifact at those coordinates",
        );
    };
    let Some(fence) = closed_html_artifact_fences(text)
        .into_iter()
        .find(|fence| fence.ordinal == fence_ordinal)
    else {
        return fail(
            conn,
            404,
            "no committed assistant artifact at those coordinates",
        );
    };

    let registry = registry();
    let session_nonce = registry.session_nonce(&session);
    let token = registry.insert(&session_nonce, seq, text_block, fence_ordinal, fence.html);
    let server = match LOOPBACK
        .get_or_try_init(|| async { LoopbackServer::bind() })
        .await
    {
        Ok(server) => server,
        Err(error) => {
            log::error!("could not start the artifact listener: {error}");
            return fail(conn, 500, "the artifact listener could not start");
        }
    };
    let resolution = Resolution {
        url: format!("/artifacts/{token}/"),
        external_url: format!("http://127.0.0.1:{}/artifacts/{token}/", server.port),
    };
    conn.with_response_header(KnownHeaderName::ContentType, "application/json")
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .with_status(200)
        .with_body(serde_json::to_string(&resolution).unwrap_or_default())
        .halt()
}

/// Token-only capability routes are mounted at the web origin root. They have no
/// session state; the coordinate resolver remains in the session control router.
pub(crate) fn routes() -> Router {
    Router::new()
        .get("/artifacts/:token/", document)
        .get("/artifacts/:token", redirect_document)
        .get("/artifacts/:token/_efference/:asset", asset)
}

pub(crate) async fn redirect_document(conn: Conn) -> Conn {
    let Some(token) = conn.param("token").map(str::to_owned) else {
        return not_found(conn);
    };
    if registry().get(&token).is_none() {
        return not_found(conn);
    }
    conn.with_response_header("location", format!("/artifacts/{token}/"))
        .with_status(308)
        .halt()
}

pub(crate) async fn document(conn: Conn) -> Conn {
    let Some(token) = conn.param("token") else {
        return not_found(conn);
    };
    let Some(html) = registry().get(token) else {
        return not_found(conn);
    };
    conn.with_response_header(KnownHeaderName::ContentType, "text/html; charset=utf-8")
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .with_response_header(KnownHeaderName::ContentSecurityPolicy, ARTIFACT_CSP)
        .with_response_header(KnownHeaderName::ReferrerPolicy, "no-referrer")
        .with_response_header("x-content-type-options", "nosniff")
        .with_response_header("x-frame-options", "SAMEORIGIN")
        .with_status(200)
        .with_body(inject_runtime(&html))
        .halt()
}

pub(crate) async fn asset(conn: Conn) -> Conn {
    let Some(token) = conn.param("token") else {
        return not_found(conn);
    };
    if registry().get(token).is_none() {
        return not_found(conn);
    }
    let Some(asset) = conn.param("asset") else {
        return not_found(conn);
    };
    let (content_type, body) = match asset {
        "runtime.js" => ("text/javascript; charset=utf-8", RUNTIME_JS),
        "theme.css" => ("text/css; charset=utf-8", THEME_CSS),
        _ => return not_found(conn),
    };
    conn.with_response_header(KnownHeaderName::ContentType, content_type)
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .with_response_header(KnownHeaderName::ReferrerPolicy, "no-referrer")
        .with_response_header("x-content-type-options", "nosniff")
        .with_status(200)
        .with_body(body)
        .halt()
}

fn inject_runtime(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(index) = lower.find("</head>") {
        return format!("{}{}{}", &html[..index], RUNTIME_TAG, &html[index..]);
    }
    if let Some(index) = lower.find("<body") {
        return format!("{}{}{}", &html[..index], RUNTIME_TAG, &html[index..]);
    }
    if let Some(index) = lower.find("<!doctype")
        && let Some(end) = html[index..].find('>')
    {
        let index = index + end + 1;
        return format!("{}\n{}{}", &html[..index], RUNTIME_TAG, &html[index..]);
    }
    format!("{RUNTIME_TAG}{html}")
}

fn fail(conn: Conn, status: u16, message: &str) -> Conn {
    conn.with_response_header(KnownHeaderName::ContentType, "application/json")
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .with_status(status)
        .with_body(serde_json::json!({ "error": message }).to_string())
        .halt()
}

fn not_found(conn: Conn) -> Conn {
    conn.with_status(404)
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .halt()
}

struct LoopbackServer {
    port: u16,
    swansong: Swansong,
}

impl LoopbackServer {
    fn bind() -> io::Result<Self> {
        let listener = StdTcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let swansong = Swansong::new();
        trillium_tokio::config()
            .without_signals()
            .with_swansong(swansong.clone())
            .listeners()
            .bind_server(listener)
            .spawn(routes());
        Ok(Self { port, swansong })
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        drop(self.swansong.shut_down());
    }
}

/// The route-path predicate shared with the TCP gate and security header middleware.
pub(super) fn is_artifact_route(path: &str) -> bool {
    let Some(mut parts) = path.strip_prefix("/artifacts/").map(|rest| rest.split('/')) else {
        return false;
    };
    let Some(token) = parts.next() else {
        return false;
    };
    if token.len() != 43
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return false;
    }
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (None, None, None)
            | (Some(""), None, None)
            | (Some("_efference"), Some("runtime.js" | "theme.css"), None)
    )
}

#[cfg(test)]
mod test;
