//! Fenced artifacts, served by signed pointer (docs/artifacts.md).
//!
//! Nothing is stored. An artifact's URL names its session, entry, text block and fence,
//! with an HMAC over them under the session's own key ([`Session::artifact_key`]), and
//! the client entry view carries it ([`links`]). Serving checks the tag, finds the
//! session among the host's, and extracts the fence from the log entry again: an
//! `html-artifact` fence is the document, and a `mermaid` fence is written into one
//! that draws it ([`mermaid_document`]). A session that leaves the host takes its key
//! with it, so its URLs stop resolving with nothing to clean up.
//!
//! The same routes are served at the web origin's root and on the artifact-only
//! loopback listener ([`spawn_listener`]), which the host starts under its `Swansong`
//! at its first adopt, once the process serving it has installed the starter
//! (`Host::set_artifact_listener`).

use std::io;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use trillium::{Conn, KnownHeaderName};
use trillium_router::{Router, RouterConnExt};

use super::{Host, Session};
use crate::policy::Verdict;
use crate::record::artifact::{Fence, Kind, fences};
use crate::record::log::{LogEntry, Payload, ToolCall};

/// The length of a session's signing key.
pub(crate) const KEY_BYTES: usize = 32;
/// An HMAC-SHA256 tag in unpadded base64url.
const TAG_LENGTH: usize = 43;
/// The largest file `efference.file` hands an artifact.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

const RUNTIME_JS: &str = include_str!("../../web/public/artifacts/runtime.js");
/// The script that draws a `mermaid` fence's diagram with the built-in library.
const MERMAID_JS: &str = include_str!("../../web/public/artifacts/mermaid.js");
/// The web client's own fonts, tokens, element defaults and prose rules, built in
/// from the files the app imports, then the artifact baseline — all but the fonts in
/// a cascade layer, so any rule an artifact writes takes precedence (docs/artifacts.md).
const THEME_CSS: &str = concat!(
    include_str!("../../web/src/theme/fonts.css"),
    "\n@layer efference {\n",
    include_str!("../../web/src/theme/tokens.css"),
    include_str!("../../web/src/theme/elements.css"),
    include_str!("../../web/src/theme/prose.css"),
    include_str!("../../web/src/theme/artifact.css"),
    "}\n",
);
/// What opens every artifact document: the stylesheet, then the runtime, which waits
/// on it.
const INJECTED: &str = "<link rel=\"stylesheet\" href=\"/artifacts/_efference/theme.css\">\
    <script src=\"/artifacts/_efference/runtime.js\"></script>";

/// Scripts may come from the page itself, the shared assets and the built-in libraries,
/// and may evaluate code: every line in an artifact is the agent's already, so `eval`
/// and WebAssembly reach nothing an inline script could not (docs/artifacts.md).
pub(super) const ARTIFACT_CSP: &str = "default-src 'none'; \
    script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src data: blob:; font-src 'self' data:; \
    connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; \
    object-src 'none'; frame-ancestors 'self'; sandbox allow-scripts";

type HmacSha256 = Hmac<Sha256>;

pub(crate) mod libraries;

/// A fresh signing key, one per [`Session`].
pub(crate) fn new_key() -> [u8; KEY_BYTES] {
    let mut key = [0; KEY_BYTES];
    rand::rng().fill_bytes(&mut key);
    key
}

/// One artifact's address, as the client entry view carries it (docs/control-api.md).
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "ArtifactLink"))]
pub(crate) struct Link {
    pub(crate) text_block: usize,
    pub(crate) fence: usize,
    pub(crate) kind: Kind,
    /// Root-relative: a web client loads it from the origin it already uses.
    pub(crate) url: String,
    /// The same document on the artifact-only loopback listener, for a local browser;
    /// absent for a session outside a host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) external_url: Option<String>,
}

/// The links of a committed assistant entry's artifacts, in order. Empty for every
/// other entry, for a record-only one, and for a session with no id to address it by.
pub(crate) fn links(session: &Session, entry: &LogEntry) -> Vec<Link> {
    let Payload::Assistant {
        output,
        excluded: None,
    } = &entry.payload
    else {
        return Vec::new();
    };
    let Some(id) = session.id() else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for (text_block, text) in output.text().into_iter().enumerate() {
        for (fence, Fence { kind, .. }) in fences(text).into_iter().enumerate() {
            let tag = tag(session.artifact_key(), id, entry.seq, text_block, fence);
            let url = format!("/artifacts/{id}/{}/{text_block}/{fence}/{tag}", entry.seq);
            let external_url = session
                .artifact_origin()
                .map(|origin| format!("{origin}{url}"));
            links.push(Link {
                text_block,
                fence,
                kind,
                url,
                external_url,
            });
        }
    }
    links
}

fn mac(
    key: &[u8; KEY_BYTES],
    session: &str,
    seq: u64,
    text_block: usize,
    fence: usize,
) -> HmacSha256 {
    let mut mac =
        <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(b"efference-html-artifact-v2\0");
    mac.update(session.as_bytes());
    mac.update(b"\0");
    for coordinate in [seq, text_block as u64, fence as u64] {
        mac.update(&coordinate.to_be_bytes());
    }
    mac
}

fn tag(key: &[u8; KEY_BYTES], session: &str, seq: u64, text_block: usize, fence: usize) -> String {
    URL_SAFE_NO_PAD.encode(
        mac(key, session, seq, text_block, fence)
            .finalize()
            .into_bytes(),
    )
}

/// The routes, mounted at the web origin's root and on the loopback listener. They sit
/// outside the device-token gate (docs/web-bind.md): a document's tag is its authority,
/// and the shared assets are as public as the web client's own.
pub(crate) fn routes() -> Router {
    Router::new()
        .get("/artifacts/_efference/:asset", asset)
        .get("/artifacts/_efference/lib/:name", libraries::library)
        .get("/artifacts/:session/:seq/:text_block/:fence/:tag", document)
        .get(
            "/artifacts/:session/:seq/:text_block/:fence/:tag/file",
            file,
        )
}

async fn document(conn: Conn) -> Conn {
    let Some(fence) = artifact_fence(&conn) else {
        return not_found(conn);
    };
    let html = match fence.kind {
        Kind::Html => fence.source,
        Kind::Mermaid => mermaid_document(&fence.source),
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

/// The session an artifact path names and the coordinates its tag was signed over,
/// when the tag verifies under that session's key.
fn verified(conn: &Conn) -> Option<(Arc<Session>, u64, usize, usize)> {
    let id = conn.param("session")?;
    let seq = conn.param("seq")?.parse().ok()?;
    let text_block = conn.param("text_block")?.parse().ok()?;
    let fence = conn.param("fence")?.parse().ok()?;
    let tag = URL_SAFE_NO_PAD.decode(conn.param("tag")?).ok()?;
    let session = session_named(conn, id)?;
    mac(session.artifact_key(), id, seq, text_block, fence)
        .verify_slice(&tag)
        .ok()?;
    Some((session, seq, text_block, fence))
}

/// The fence a document path names, when its tag verifies and the session's log holds
/// the fence.
fn artifact_fence(conn: &Conn) -> Option<Fence> {
    let (session, seq, text_block, fence) = verified(conn)?;
    let entry = session.entry_at(seq)?;
    let Payload::Assistant {
        output,
        excluded: None,
    } = entry.payload
    else {
        return None;
    };
    let text = output.text().into_iter().nth(text_block)?;
    fences(text).into_iter().nth(fence)
}

/// The page a `mermaid` fence is drawn in: the source as a JSON string, then the script
/// that draws it with the built-in library (docs/artifacts.md). Every `<` in the JSON is
/// escaped, so no source can end the element it sits in.
fn mermaid_document(source: &str) -> String {
    let source = serde_json::to_string(source)
        .expect("a string serializes")
        .replace('<', "\\u003c");
    format!(
        "<!doctype html><div id=\"efference-mermaid\"></div>\
         <script type=\"application/json\" id=\"efference-mermaid-source\">{source}</script>\
         <script src=\"/artifacts/_efference/mermaid.js\"></script>"
    )
}

/// The session a document path names: one of the host's, or the one a session bind
/// serves.
fn session_named(conn: &Conn, id: &str) -> Option<Arc<Session>> {
    match conn.shared_state::<Arc<Host>>() {
        Some(host) => host.session(id),
        None => super::http::shared_of(conn).filter(|session| session.id() == Some(id)),
    }
}

/// A file from the session's working directory for the artifact the path names
/// (`efference.file`, docs/artifacts.md), as a script handing the runtime its text or
/// the reason it is withheld. Read from disk on every request: the data is never
/// projected and the agent never sees it, so it may change under an old transcript, and
/// the chart with it.
async fn file(conn: Conn) -> Conn {
    let Some((session, ..)) = verified(&conn) else {
        return not_found(conn);
    };
    let path = trillium_client::url::form_urlencoded::parse(conn.querystring().as_bytes())
        .find(|(key, _)| key == "path")
        .map_or_default(|(_, value)| value.into_owned());
    let script = match file_text(&session, &path).await {
        Ok(text) => format!(
            "efference.__file({}, {});\n",
            serde_json::json!(path),
            serde_json::json!(text)
        ),
        Err(reason) => format!(
            "efference.__fileRefused({}, {});\n",
            serde_json::json!(path),
            serde_json::json!(reason)
        ),
    };
    conn.with_response_header(
        KnownHeaderName::ContentType,
        "text/javascript; charset=utf-8",
    )
    .with_response_header(KnownHeaderName::CacheControl, "no-store")
    .with_response_header("x-content-type-options", "nosniff")
    .with_status(200)
    .with_body(script)
    .halt()
}

/// The text of `path` for one of `session`'s artifacts, or why it is withheld, in words
/// the page shows the operator. Only what the session's own `read` of that path would
/// do without asking: an artifact sees nothing the agent could not read and inline.
async fn file_text(session: &Session, path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err("no path given".into());
    }
    let read = ToolCall::new(
        "read",
        "artifact-file",
        serde_json::json!({ "paths": [path] }),
    );
    let verdict =
        crate::policy::classify(&session.directories(), &session.effective_config(), &read);
    if !matches!(verdict, Verdict::Clear) {
        return Err("this session cannot read it without asking".into());
    }
    let full = session.cwd().join(path);
    let metadata = tokio::fs::metadata(&full)
        .await
        .map_err(|_| "no such file".to_owned())?;
    if !metadata.is_file() {
        return Err("not a file".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("larger than {} MB", MAX_FILE_BYTES / (1024 * 1024)));
    }
    let bytes = tokio::fs::read(&full)
        .await
        .map_err(|e| format!("could not be read: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_owned())
}

async fn asset(conn: Conn) -> Conn {
    let (content_type, body) = match conn.param("asset") {
        Some("runtime.js") => ("text/javascript; charset=utf-8", RUNTIME_JS),
        Some("mermaid.js") => ("text/javascript; charset=utf-8", MERMAID_JS),
        Some("theme.css") => ("text/css; charset=utf-8", THEME_CSS),
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

/// Put the stylesheet and the runtime first in the document, right after its doctype,
/// adding `<!doctype html>` when the source has none (docs/artifacts.md). A fragment with no
/// doctype renders in quirks mode, which stretches the root element to the frame and
/// leaves the runtime no content height to report. A script before `<html>` is valid
/// in any document: the parser opens the head for it, and an authored `<html>` or
/// `<head>` tag later merges into the elements already open.
fn inject_runtime(html: &str) -> String {
    let html = html.strip_prefix('\u{feff}').unwrap_or(html);
    match doctype_end(html) {
        Some(end) => format!("{}{INJECTED}{}", &html[..end], &html[end..]),
        None => format!("<!doctype html>{INJECTED}{html}"),
    }
}

/// The offset just past a doctype that opens the document — preceded by nothing but
/// whitespace and comments, the only place the parser honours one.
fn doctype_end(html: &str) -> Option<usize> {
    let mut rest = html;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let Some(comment) = rest.strip_prefix("<!--") else {
            break;
        };
        rest = &comment[comment.find("-->")? + 3..];
    }
    if !rest.get(..9)?.eq_ignore_ascii_case("<!doctype") {
        return None;
    }
    Some(html.len() - rest.len() + rest.find('>')? + 1)
}

fn not_found(conn: Conn) -> Conn {
    conn.with_status(404)
        .with_response_header(KnownHeaderName::CacheControl, "no-store")
        .halt()
}

/// Bind the artifact-only loopback listener and serve [`routes`] on it under `host`'s
/// `Swansong`, with the host as its state, so host shutdown ends it with everything
/// else. Answers the listener's origin. The starter a serving process installs on its
/// host (`Host::set_artifact_listener`).
pub fn spawn_listener(host: &Arc<Host>) -> io::Result<String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    trillium_tokio::config()
        .without_signals()
        .with_swansong(host.swansong().clone())
        .with_shared_state(Arc::clone(host))
        .listeners()
        .bind_server(listener)
        .spawn((super::tcp::SecurityHeaders::new(false), routes(), fonts()));
    Ok(format!("http://127.0.0.1:{port}"))
}

/// The web client's fonts, which the stylesheet names at `/fonts/`, served from the
/// build the web origin embeds; nothing else of the client's reaches this listener.
/// Not under `dev-proxy`, where each frontend handler starts a dev server of its own:
/// there the stylesheet's fonts fall back to the stacks' system faces.
fn fonts() -> impl trillium::Handler {
    #[cfg(not(feature = "dev-proxy"))]
    let frontend = Some(super::tcp::frontend(super::tcp::Surface::Session));
    #[cfg(feature = "dev-proxy")]
    let frontend: Option<()> = None;
    FontsOnly(frontend)
}

struct FontsOnly<H>(Option<H>);

impl<H: trillium::Handler> trillium::Handler for FontsOnly<H> {
    async fn run(&self, conn: Conn) -> Conn {
        match &self.0 {
            Some(frontend) if conn.path().starts_with("/fonts/") => frontend.run(conn).await,
            _ => conn,
        }
    }

    async fn init(&mut self, info: &mut trillium::Info) {
        if let Some(frontend) = &mut self.0 {
            frontend.init(info).await;
        }
    }
}

/// Whether `path` is an artifact route — the gate's anonymous exception and the
/// security headers' policy switch. Exact shapes only, so nothing else under
/// `/artifacts/` slips past the gate.
pub(super) fn is_artifact_route(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/artifacts/") else {
        return false;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let number = |segment: &str| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit());
    match segments.as_slice() {
        ["_efference", "runtime.js" | "mermaid.js" | "theme.css"] => true,
        ["_efference", "lib", name] => {
            !matches!(*name, "" | "." | "..")
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        }
        [session, seq, text_block, fence, tag] | [session, seq, text_block, fence, tag, "file"] => {
            !matches!(*session, "" | "." | ".." | "_efference")
                && session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                && number(seq)
                && number(text_block)
                && number(fence)
                && tag.len() == TAG_LENGTH
                && tag
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        }
        _ => false,
    }
}

#[cfg(test)]
mod test;
