//! The streamable-HTTP transport (docs/tools.md "connected providers"):
//! JSON-RPC POSTed to a single endpoint over `trillium-client`.
//!
//! Each request is one POST. The server answers either `application/json`
//! (one JSON-RPC response) or `text/event-stream` (a per-request SSE stream
//! that may interleave notifications before the response arrives; the
//! response ends our interest in the stream). The branch is driven through
//! `into_sse`: the Ok arm is the event stream (response headers reachable via
//! `EventStream::conn()`), and an `UnexpectedContentType` error *is* the JSON
//! arm — the `SseError` hands the executed conn back.
//!
//! Session and version state ride as headers once learned: `Mcp-Session-Id`
//! (issued by the server, echoed on every later request) and
//! `MCP-Protocol-Version` (the negotiated revision, per `2025-06-18`+).
//! A `401` is surfaced as "authorization required" — the OAuth flow is a
//! planned unit, not yet built.

use anyhow::Context;
use futures_lite::StreamExt;
use mcplease::types::{
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use serde_json::Value;
use trillium_client::sse::SseErrorKind;
use trillium_client::{Client as HttpClient, Conn};
use trillium_rustls::RustlsConfig;
use trillium_tokio::ClientConfig;

pub struct HttpTransport {
    client: HttpClient,
    url: String,
    /// Issued by the server (initialize response header) and echoed back on
    /// every subsequent request until the session ends.
    session_id: Option<String>,
    /// The negotiated protocol revision, carried as a header after the
    /// handshake settles it.
    protocol_version: Option<String>,
    next_id: i64,
}

impl HttpTransport {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: HttpClient::new(RustlsConfig::<ClientConfig>::default()),
            url: url.to_string(),
            session_id: None,
            protocol_version: None,
            next_id: 0,
        })
    }

    pub fn set_protocol_version(&mut self, version: &str) {
        self.protocol_version = Some(version.to_string());
    }

    /// An unexecuted POST carrying the transport's standing headers.
    fn base_conn(&self) -> Conn {
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
        conn
    }

    /// Learn the session id if this response issued or refreshed one.
    fn learn_session(&mut self, conn: &Conn) {
        if let Some(session_id) = conn.response_headers().get_str("mcp-session-id") {
            self.session_id = Some(session_id.to_string());
        }
    }

    pub async fn request(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = RequestId::Integer(self.next_id);
        let request = JsonRpcRequest::new(id.clone(), method, params);
        let body = serde_json::to_string(&JsonRpcMessage::Request(request))?;

        match self.base_conn().with_body(body).into_sse().await {
            // The SSE arm: read events until our response id shows up.
            // Interleaved notifications and server-initiated requests are
            // logged and skipped (unanswered — efference advertised no client
            // capabilities); a stream that ends without the response is an
            // error, and the caller must re-issue rather than resume.
            Ok(mut events) => {
                let session_id = events
                    .conn()
                    .response_headers()
                    .get_str("mcp-session-id")
                    .map(String::from);
                if let Some(session_id) = session_id {
                    self.session_id = Some(session_id);
                }
                while let Some(event) = events.next().await {
                    let event = event.with_context(|| format!("reading {method} events"))?;
                    match serde_json::from_str::<JsonRpcMessage>(event.data()) {
                        Ok(JsonRpcMessage::Response(response))
                            if response.id.as_ref() == Some(&id) =>
                        {
                            return response
                                .into_result()
                                .map_err(|e| anyhow::anyhow!("{method}: {e}"));
                        }
                        Ok(JsonRpcMessage::Response(stale)) => {
                            log::debug!("skipping response to abandoned request {:?}", stale.id);
                        }
                        Ok(JsonRpcMessage::Notification(notification)) => {
                            log::debug!("ignoring server notification {}", notification.method);
                        }
                        Ok(JsonRpcMessage::Request(request)) => {
                            log::debug!(
                                "ignoring server-initiated request {} (no client capabilities)",
                                request.method
                            );
                        }
                        Err(e) => log::debug!("ignoring unparseable SSE data: {e}"),
                    }
                }
                anyhow::bail!("event stream ended before the {method} response arrived")
            }
            Err(e) => match &e.kind {
                // The JSON arm: a non-event-stream 2xx is the response itself.
                SseErrorKind::UnexpectedContentType(_) => {
                    let mut conn: Conn = e.into();
                    self.learn_session(&conn);
                    let body = conn.response_body().read_string().await?;
                    let response: JsonRpcResponse = serde_json::from_str(&body)
                        .with_context(|| format!("unparseable {method} response"))?;
                    response
                        .into_result()
                        .map_err(|e| anyhow::anyhow!("{method}: {e}"))
                }
                SseErrorKind::Status(status) => {
                    let status = *status;
                    let mut conn: Conn = e.into();
                    let body = conn.response_body().read_string().await.unwrap_or_default();
                    if u16::from(status) == 401 {
                        anyhow::bail!(
                            "server requires authorization (401); the OAuth flow is not yet \
                             implemented"
                        );
                    }
                    anyhow::bail!("server returned {status}: {body}")
                }
                other => Err(anyhow::anyhow!("POST {}: {other}", self.url)),
            },
        }
    }

    pub async fn notify(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        let notification = JsonRpcNotification::new(method, params);
        let body = serde_json::to_string(&JsonRpcMessage::Notification(notification))?;
        let conn = self
            .base_conn()
            .with_body(body)
            .await
            .with_context(|| format!("POST {}", self.url))?;
        self.learn_session(&conn);
        // Expected 202 Accepted; any 2xx is taken as delivered.
        let status = conn.status().map_or(0, u16::from);
        if !(200..300).contains(&status) {
            anyhow::bail!("server returned {status} to a notification");
        }
        Ok(())
    }
}
