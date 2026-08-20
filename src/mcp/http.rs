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
//! specific tool, prompt, or resource (SEP-2243). The derivation (and its
//! value encoding) is `mcplease::headers::standard_headers` — the same code a
//! server validates with, so the two cannot drift; `base_conn` attaches them
//! in the one place that already holds the message. A server that enforces
//! them rejects a disagreement with `HeaderMismatch`.

use anyhow::Context;
use futures_lite::StreamExt;
use mcplease::types::JsonRpcMessage;
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
        for (name, value) in mcplease::headers::standard_headers(message) {
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
