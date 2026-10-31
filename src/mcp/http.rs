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
//! `MCP-Protocol-Version` (the negotiated revision, per `2025-06-18`+).
//!
//! A transport mounted with stored credentials sends their header on every
//! request and renews them (docs/mcp-auth.md): before a request when the grant
//! is past its expiry, and once when the server answers `401`, sending the
//! request again with the renewed grant. A `401` that renewal cannot answer —
//! no credentials, a failed renewal, or a renewed grant refused as well —
//! becomes `crate::mcp::auth::AuthorizationRequired`, which the boot step acts
//! on and a call reports. Nothing is sent a third time.
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

use crate::mcp::auth::{AuthorizationRequired, Credentials};
use crate::www_authenticate::Challenges;

pub struct HttpTransport {
    client: HttpClient,
    url: String,
    /// The stored credentials whose header goes on every request, once the
    /// server has been authorized (docs/mcp-auth.md). The spec requires it on
    /// *every* request, not just the first. Boxed to keep the transport, and the
    /// `Transport` enum it is a variant of, near the stdio transport's size.
    credentials: Option<Box<Credentials>>,
    /// Issued by the server (initialize response header) and echoed back on
    /// every subsequent request until the session ends.
    session_id: Option<String>,
    /// The negotiated protocol revision, carried as a header after
    /// negotiation settles it.
    protocol_version: Option<String>,
}

impl HttpTransport {
    pub fn new(url: &str) -> Self {
        Self {
            client: crate::http::client(),
            url: url.to_string(),
            credentials: None,
            session_id: None,
            protocol_version: None,
        }
    }

    /// Send these credentials on every request, renewing them as they lapse.
    pub fn with_credentials(mut self, credentials: Option<Credentials>) -> Self {
        self.credentials = credentials.map(Box::new);
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
        if let Some(credentials) = &self.credentials {
            conn = conn.with_request_header("authorization", credentials.header());
        }
        conn
    }

    /// Renew the credentials before a request if their grant is past its expiry.
    async fn renew_expired(&mut self) -> Result<(), AuthorizationRequired> {
        match &mut self.credentials {
            Some(credentials) if credentials.is_expired() => {
                credentials
                    .renew()
                    .await
                    .map_err(|why| AuthorizationRequired {
                        challenge: None,
                        renewal_failed: Some(why),
                    })
            }
            _ => Ok(()),
        }
    }

    /// A request was answered `401`: renew the credentials so it can be sent
    /// once more, or hand back what the server said when there are none to
    /// renew or the renewal fails.
    async fn renew_refused(
        &mut self,
        refused: AuthorizationRequired,
    ) -> Result<(), AuthorizationRequired> {
        let Some(credentials) = &mut self.credentials else {
            return Err(refused);
        };
        credentials
            .renew()
            .await
            .map_err(|why| AuthorizationRequired {
                challenge: refused.challenge,
                renewal_failed: Some(why),
            })
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
        self.renew_expired().await?;
        let refused = match self.post(message, &body).await? {
            Posted::Answered(exchange) => return Ok(exchange),
            Posted::Unauthorized(refused) => refused,
        };
        self.renew_refused(refused).await?;
        match self.post(message, &body).await? {
            Posted::Answered(exchange) => Ok(exchange),
            Posted::Unauthorized(refused) => Err(refused_after_renewal(refused).into()),
        }
    }

    /// One POST of `body`, whose answer is either the exchange or a `401`.
    async fn post(
        &mut self,
        message: &JsonRpcMessage,
        body: &str,
    ) -> anyhow::Result<Posted<HttpExchange>> {
        match self
            .base_conn(message)
            .with_body(body.to_string())
            .into_sse()
            .await
        {
            Ok(events) => {
                let session_id = events
                    .conn()
                    .response_headers()
                    .get_str("mcp-session-id")
                    .map(String::from);
                if let Some(session_id) = session_id {
                    self.session_id = Some(session_id);
                }
                Ok(Posted::Answered(HttpExchange::Events(Box::new(events))))
            }
            Err(e) => match &e.kind {
                // The JSON arm: a non-event-stream 2xx is the response itself.
                SseErrorKind::UnexpectedContentType(_) => {
                    let mut conn: Conn = e.into();
                    self.learn_session(&conn);
                    let body = conn.response_body().read_string().await?;
                    let message = serde_json::from_str(&body)
                        .with_context(|| format!("unparseable response body: {body}"))?;
                    Ok(Posted::Answered(HttpExchange::Once(Some(message))))
                }
                SseErrorKind::Status(status) => {
                    let status = *status;
                    let mut conn: Conn = e.into();
                    let body = conn.response_body().read_string().await.unwrap_or_default();
                    if u16::from(status) == 401 {
                        return Ok(Posted::Unauthorized(authorization_required(&conn)));
                    }
                    anyhow::bail!("server returned {status}: {body}")
                }
                other => Err(anyhow::anyhow!("POST {}: {other}", self.url)),
            },
        }
    }

    /// POST a one-way message. Expected `202 Accepted`; any 2xx is taken as
    /// delivered. Credentials are renewed as [`open`](Self::open) renews them.
    pub async fn notify(&mut self, message: &JsonRpcMessage) -> anyhow::Result<()> {
        let body = serde_json::to_string(message)?;
        self.renew_expired().await?;
        let refused = match self.deliver(message, &body).await? {
            Posted::Answered(()) => return Ok(()),
            Posted::Unauthorized(refused) => refused,
        };
        self.renew_refused(refused).await?;
        match self.deliver(message, &body).await? {
            Posted::Answered(()) => Ok(()),
            Posted::Unauthorized(refused) => Err(refused_after_renewal(refused).into()),
        }
    }

    /// One POST of a notification's `body`.
    async fn deliver(
        &mut self,
        message: &JsonRpcMessage,
        body: &str,
    ) -> anyhow::Result<Posted<()>> {
        let conn = self
            .base_conn(message)
            .with_body(body.to_string())
            .await
            .with_context(|| format!("POST {}", self.url))?;
        self.learn_session(&conn);
        let status = conn.status().map_or(0, u16::from);
        if status == 401 {
            return Ok(Posted::Unauthorized(authorization_required(&conn)));
        }
        if !(200..300).contains(&status) {
            anyhow::bail!("server returned {status} to a notification");
        }
        Ok(Posted::Answered(()))
    }
}

/// What one POST came back with: the answer, or a `401` that renewing the
/// credentials may get past.
enum Posted<T> {
    Answered(T),
    Unauthorized(AuthorizationRequired),
}

/// A request refused again after its credentials were renewed. Not renewed a
/// second time, so a server that refuses every grant costs a call one refresh
/// rather than a loop.
fn refused_after_renewal(refused: AuthorizationRequired) -> AuthorizationRequired {
    AuthorizationRequired {
        renewal_failed: Some("the server refused the renewed grant too".to_string()),
        ..refused
    }
}

/// Read the `WWW-Authenticate` challenge off a `401` into the typed error the
/// boot step acts on (docs/mcp-auth.md), before any renewal has been tried.
///
/// A header that appears more than once is one list (RFC 9110 §5.3), so every
/// value is parsed together rather than only the first. A server that sends no
/// challenge, or an unparseable one, still yields the error — discovery has
/// well-known-URI fallbacks for everything the challenge would have told us.
fn authorization_required(conn: &Conn) -> AuthorizationRequired {
    let values = conn.response_headers().get_values("www-authenticate");
    let challenges = match values {
        Some(values) => Challenges::parse_all(values.iter().filter_map(HeaderValue::as_str)),
        None => Challenges::default(),
    };
    AuthorizationRequired {
        challenge: challenges.scheme("Bearer").cloned(),
        renewal_failed: None,
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

#[cfg(test)]
mod test;
