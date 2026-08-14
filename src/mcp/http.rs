//! The streamable-HTTP transport (docs/tools.md "connected providers"):
//! JSON-RPC POSTed to a single endpoint over `trillium-client`.
//!
//! Framing only, like the stdio transport — the protocol decisions live in
//! `mcplease::client`. What is genuinely this transport's own is the shape of
//! an exchange: each request is one POST, and the server answers either
//! `application/json` (one JSON-RPC response) or `text/event-stream` (a
//! per-request SSE stream that may interleave notifications before the
//! response arrives). Both are modeled as [`HttpExchange`], a source of frames
//! that runs dry when the server is done talking.
//!
//! The branch is driven through `into_sse`: the Ok arm is the event stream
//! (response headers reachable via `EventStream::conn()`), and an
//! `UnexpectedContentType` error *is* the JSON arm — the `SseError` hands the
//! executed conn back.
//!
//! Session and version state ride as headers once learned: `Mcp-Session-Id`
//! (issued by the server, echoed on every later request) and
//! `MCP-Protocol-Version` (the negotiated revision, per `2025-06-18`+). A `401`
//! becomes `crate::mcp::auth::AuthorizationRequired`, which the boot phase acts
//! on (docs/mcp-auth.md).
//!
//! `2026-07-28` adds two *required* headers derived from the body —
//! `Mcp-Method` on every request and `Mcp-Name` on the methods that address a
//! specific tool, prompt, or resource (SEP-2243). They are derived here, in the
//! one place that already holds the message, because the whole point of them is
//! that they agree with the body; a server that enforces them rejects a
//! disagreement with `HeaderMismatch`.

use anyhow::Context;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_lite::StreamExt;
use mcplease::types::JsonRpcMessage;
use serde_json::Value;
use trillium_client::sse::{EventStream, SseErrorKind};
use trillium_client::{Client as HttpClient, Conn, HeaderValue};

use crate::www_authenticate::Challenges;
use trillium_rustls::RustlsConfig;
use trillium_tokio::ClientConfig;

pub struct HttpTransport {
    client: HttpClient,
    url: String,
    /// The `Authorization` header value to send on every request, once the
    /// server has been authorized (docs/mcp-auth.md). The spec requires it on
    /// *every* request, not just the first.
    authorization: Option<String>,
    /// Issued by the server (initialize response header) and echoed back on
    /// every subsequent request until the session ends.
    session_id: Option<String>,
    /// The negotiated protocol revision, carried as a header after
    /// negotiation settles it.
    protocol_version: Option<String>,
}

impl HttpTransport {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: HttpClient::new(RustlsConfig::<ClientConfig>::default()),
            url: url.to_string(),
            authorization: None,
            session_id: None,
            protocol_version: None,
        })
    }

    /// Send this `Authorization` header on every request.
    pub fn with_authorization(mut self, authorization: Option<&str>) -> Self {
        self.authorization = authorization.map(String::from);
        self
    }

    pub fn set_protocol_version(&mut self, version: &str) {
        self.protocol_version = Some(version.to_string());
    }

    /// An unexecuted POST carrying the transport's standing headers, plus the
    /// per-message standard headers `2026-07-28` requires.
    fn base_conn(&self, message: &JsonRpcMessage) -> Conn {
        let mut conn = self.standing_conn();
        for (name, value) in standard_headers(message) {
            conn = conn.with_request_header(name, value);
        }
        conn
    }

    /// The headers that do not depend on the message being sent.
    fn standing_conn(&self) -> Conn {
        let mut conn = self
            .client
            .post(self.url.as_str())
            .with_request_header("content-type", "application/json")
            .with_request_header("accept", "application/json, text/event-stream");
        if let Some(session_id) = &self.session_id {
            conn = conn.with_request_header("mcp-session-id", session_id.clone());
        }
        if let Some(version) = &self.protocol_version {
            conn = conn.with_request_header("mcp-protocol-version", version.clone());
        }
        if let Some(authorization) = &self.authorization {
            conn = conn.with_request_header("authorization", authorization.clone());
        }
        conn
    }

    /// Learn the session id if this response issued or refreshed one.
    fn learn_session(&mut self, conn: &Conn) {
        if let Some(session_id) = conn.response_headers().get_str("mcp-session-id") {
            self.session_id = Some(session_id.to_string());
        }
    }

    /// POST a message that expects an answer, and hand back the frames the
    /// server sends in response to it.
    pub async fn open(&mut self, message: &JsonRpcMessage) -> anyhow::Result<HttpExchange> {
        let body = serde_json::to_string(message)?;
        match self.base_conn(message).with_body(body).into_sse().await {
            Ok(events) => {
                let session_id = events
                    .conn()
                    .response_headers()
                    .get_str("mcp-session-id")
                    .map(String::from);
                if let Some(session_id) = session_id {
                    self.session_id = Some(session_id);
                }
                Ok(HttpExchange::Events(Box::new(events)))
            }
            Err(e) => match &e.kind {
                // The JSON arm: a non-event-stream 2xx is the response itself.
                SseErrorKind::UnexpectedContentType(_) => {
                    let mut conn: Conn = e.into();
                    self.learn_session(&conn);
                    let body = conn.response_body().read_string().await?;
                    let message = serde_json::from_str(&body)
                        .with_context(|| format!("unparseable response body: {body}"))?;
                    Ok(HttpExchange::Once(Some(message)))
                }
                SseErrorKind::Status(status) => {
                    let status = *status;
                    let mut conn: Conn = e.into();
                    let body = conn.response_body().read_string().await.unwrap_or_default();
                    if u16::from(status) == 401 {
                        return Err(authorization_required(&conn).into());
                    }
                    anyhow::bail!("server returned {status}: {body}")
                }
                other => Err(anyhow::anyhow!("POST {}: {other}", self.url)),
            },
        }
    }

    /// POST a one-way message. Expected `202 Accepted`; any 2xx is taken as
    /// delivered.
    pub async fn notify(&mut self, message: &JsonRpcMessage) -> anyhow::Result<()> {
        let body = serde_json::to_string(message)?;
        let conn = self
            .base_conn(message)
            .with_body(body)
            .await
            .with_context(|| format!("POST {}", self.url))?;
        self.learn_session(&conn);
        let status = conn.status().map_or(0, u16::from);
        if !(200..300).contains(&status) {
            anyhow::bail!("server returned {status} to a notification");
        }
        Ok(())
    }
}

/// The standard request headers `2026-07-28` requires on every streamable-HTTP
/// POST (SEP-2243). `Mcp-Method` mirrors the body's `method` on all requests;
/// `Mcp-Name` mirrors `params.name` or `params.uri` on the three methods that
/// address a specific thing.
///
/// They are **required for compliance**, and a server that enforces them
/// rejects the request with a `HeaderMismatch` naming the disagreement rather
/// than failing obscurely — which is how their absence was found. Servers
/// differ on whether they enforce: ferritin does not, Sentry does.
///
/// Deriving them here rather than at the call sites is the point: the headers
/// have to agree with the body, so the one place that already has the body is
/// the only place they can be derived without drifting.
pub(super) fn standard_headers(message: &JsonRpcMessage) -> Vec<(&'static str, String)> {
    let (method, params) = match message {
        JsonRpcMessage::Request(request) => (&request.method, &request.params),
        JsonRpcMessage::Notification(notification) => (&notification.method, &notification.params),
        // A response has no `method`; these headers describe a request.
        JsonRpcMessage::Response(_) => return Vec::new(),
    };
    let mut headers = vec![("mcp-method", encode_header_value(method))];
    if let Some(name) = mcp_name(method, params.as_ref()) {
        headers.push(("mcp-name", encode_header_value(&name)));
    }
    headers
}

/// `Mcp-Name`'s source field: `params.name` for a tool or prompt, `params.uri`
/// for a resource. No other method carries it.
pub(super) fn mcp_name(method: &str, params: Option<&Value>) -> Option<String> {
    let field = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" => "uri",
        _ => return None,
    };
    Some(params?.get(field)?.as_str()?.to_string())
}

const SENTINEL_PREFIX: &str = "=?base64?";
const SENTINEL_SUFFIX: &str = "?=";

/// A header value per the spec's Value Encoding rules: as-is when it is safe to
/// send raw, and `=?base64?<standard base64 of the UTF-8 bytes>?=` when it is
/// not.
///
/// A value that *looks* encoded but is not gets encoded too, so the server
/// cannot mistake a literal `=?base64?…?=` for a marker — the one rule here
/// that exists purely to remove an ambiguity rather than to carry bytes safely.
pub(super) fn encode_header_value(value: &str) -> String {
    if is_header_safe(value) {
        return value.to_string();
    }
    format!(
        "{SENTINEL_PREFIX}{}{SENTINEL_SUFFIX}",
        BASE64.encode(value.as_bytes())
    )
}

/// Whether a value can ride in a header raw: visible ASCII, space, or tab
/// (RFC 9110), no leading or trailing whitespace to be stripped in transit, and
/// not something a server would read as an encoding marker.
fn is_header_safe(value: &str) -> bool {
    let looks_encoded = value.starts_with(SENTINEL_PREFIX) && value.ends_with(SENTINEL_SUFFIX);
    !looks_encoded
        && value
            .bytes()
            .all(|b| (0x20..=0x7e).contains(&b) || b == 0x09)
        && !value.starts_with([' ', '\t'])
        && !value.ends_with([' ', '\t'])
}

/// Read the `WWW-Authenticate` challenge off a `401` into the typed error the
/// boot phase acts on (docs/mcp-auth.md).
///
/// A header that appears more than once is one list (RFC 9110 §5.3), so every
/// value is parsed together rather than only the first. A server that sends no
/// challenge, or an unparseable one, still yields the error — discovery has
/// well-known-URI fallbacks for everything the challenge would have told us.
fn authorization_required(conn: &Conn) -> crate::mcp::auth::AuthorizationRequired {
    let values = conn.response_headers().get_values("www-authenticate");
    let challenges = match values {
        Some(values) => Challenges::parse_all(values.iter().filter_map(HeaderValue::as_str)),
        None => Challenges::default(),
    };
    crate::mcp::auth::AuthorizationRequired {
        challenge: challenges.scheme("Bearer").cloned(),
    }
}

/// The frames one POST produced: a single JSON response, or an event stream
/// that may carry several messages before the one being waited on.
pub enum HttpExchange {
    Once(Option<JsonRpcMessage>),
    // Boxed: an `EventStream` is far larger than the `Once` arm.
    Events(Box<EventStream>),
}

impl HttpExchange {
    /// The next frame, or an error once the exchange has run dry — a POST
    /// whose answer never arrived cannot be resumed, only re-issued.
    pub async fn next_frame(&mut self) -> anyhow::Result<JsonRpcMessage> {
        match self {
            Self::Once(message) => message
                .take()
                .ok_or_else(|| anyhow::anyhow!("the response body held only one message")),
            Self::Events(events) => loop {
                let Some(event) = events.next().await else {
                    anyhow::bail!("the event stream ended before the response arrived");
                };
                let event = event.context("reading events")?;
                match serde_json::from_str(event.data()) {
                    Ok(message) => return Ok(message),
                    Err(e) => log::debug!("ignoring unparseable SSE data: {e}"),
                }
            },
        }
    }
}
