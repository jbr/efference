//! The streaming client: `complete` and the cache `keepalive` (docs/transport.md).

use super::*;

use std::time::Duration;

use trillium::Status;
use trillium_client_retry::RetryHandler;

/// Retry policy for requests that did not receive a successful response
/// (docs/transport.md). Every upstream request is a replay-safe POST carrying
/// an owned `String` body. The elapsed ceiling bounds an outage from the
/// operator's perspective; exhaustion still surfaces the final response.
const RETRY_MAX_ATTEMPTS: u32 = 4;
const RETRY_MAX_ELAPSED: Duration = Duration::from_secs(30);

/// A thin Anthropic Messages client
/// auth), the model, and the stable system prompt. The system is byte-stable across
/// the session, so it stays in the cached prefix (docs/log-and-projection.md); the fork
/// (docs/redaction-fork.md) shares the same endpoint and system, so its prefix matches
/// and hits cache.
pub struct Client {
    http: HttpClient,
    endpoint: Endpoint,
    model: String,
    system: String,
}

impl Client {
    pub fn new(endpoint: Endpoint, model: impl Into<String>, system: impl Into<String>) -> Self {
        Self {
            // TLS over the tokio TCP connector; ALPN negotiates HTTP/2 (no h3
            // advertised by Anthropic/Cloudflare as of 2026-06). See main.rs.
            http: HttpClient::new(RustlsConfig::<ClientConfig>::default()).with_handler(
                RetryHandler::default()
                    // Messages POSTs are replay-safe: an unsuccessful open produced no
                    // assistant entry or provider-side completion.
                    .with_all_methods()
                    // `with_statuses` replaces the default (429, 503), so enumerate the
                    // whole transient set. Other 4xx responses remain immediate errors.
                    .with_statuses([
                        Status::TooManyRequests,
                        Status::InternalServerError,
                        Status::BadGateway,
                        Status::ServiceUnavailable,
                        Status::GatewayTimeout,
                    ])
                    .with_max_attempts(RETRY_MAX_ATTEMPTS)
                    .with_max_elapsed(RETRY_MAX_ELAPSED)
                    .with_max_retry_after(RETRY_MAX_ELAPSED),
            ),
            endpoint,
            model: model.into(),
            system: system.into(),
        }
    }

    /// One streaming round-trip: serialize `messages`, POST with `stream: true`,
    /// and fold the SSE event stream back into a [`Completion`]. Each live signal — prose,
    /// thinking, a tool call opening, its arguments arriving — is handed to `on_delta` as a
    /// [`Streamed`] the moment it arrives: the transport-only live sink (the daemon publishes it to
    /// `GET /stream`). Pass a no-op `|_| {}` when nothing is listening (e.g. the fork peek, whose
    /// triage tokens must never surface).
    ///
    /// `into_sse` *is* the execution — the conn must not be awaited first (that
    /// is `SseErrorKind::AlreadyExecuted`). On a non-2xx it returns
    /// `SseErrorKind::Status` carrying the code; the failed `SseError` still owns the
    /// `Conn` (`SseError: Into<Conn>`), so [`Self::sse_open_error`] reads the response
    /// body off it — Anthropic's 4xx carry a JSON `error.message` naming exactly what
    /// was malformed (e.g. an oversized request that exceeds the input limit), which is
    /// what makes such failures diagnosable.
    pub async fn complete(
        &self,
        messages: &[ApiMessage],
        tools: &[Value],
        tool_choice: ToolChoice,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        let request = Request {
            model: &self.model,
            max_tokens: MAX_TOKENS,
            system: &self.system,
            tools,
            tool_choice,
            thinking: Thinking::for_request(&self.model, &tool_choice),
            messages,
            stream: true,
        };
        let body = serde_json::to_string(&request)?;
        log::debug!(
            "POST {}: {} messages, {} request bytes",
            self.endpoint.url,
            messages.len(),
            body.len()
        );

        let mut events = match self
            .http
            .post(self.endpoint.url.as_str())
            .with_request_header(
                self.endpoint.auth_header.clone(),
                self.endpoint.auth_value.clone(),
            )
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json")
            .with_body(body)
            .into_sse()
            .await
        {
            Ok(events) => events,
            Err(e) => return Err(Self::sse_open_error(e).await),
        };

        let mut assembler = Assembler::new();
        while let Some(event) = events.next().await {
            let event = event?;
            if let Some(streamed) = assembler.apply(event.event_type(), event.data())? {
                on_delta(streamed);
            }
        }
        assembler.finish()
    }

    /// Turn a failed `into_sse` into a legible [`Error`], reading the response body on a
    /// non-2xx. The `SseError` still owns the `Conn` (it `Deref`s to it and converts
    /// back via `Into<Conn>`), so on `Status` we read the body — Anthropic's error
    /// responses carry a JSON `error.message` that names the actual problem. Logged at
    /// `error` so the daemon's log preserves it even though the error also propagates.
    ///
    /// Note: this still *propagates* (the caller currently treats any completion error
    /// as terminal). Classifying transient statuses (429 / 529 overload, 5xx) as
    /// retryable rather than fatal is a separate, deferred change; this only makes the
    /// failure diagnosable.
    async fn sse_open_error(e: SseError) -> Error {
        // `Status` is `Copy`, so binding it doesn't move `e.kind` — `e` stays whole and
        // convertible into its `Conn`.
        let message = if let SseErrorKind::Status(status) = e.kind {
            let mut conn: Conn = e.into();
            let body = conn
                .response_body()
                .read_string()
                .await
                .unwrap_or_else(|read_err| format!("(could not read error body: {read_err})"));
            format!("upstream API returned {status}: {body}")
        } else {
            format!("upstream SSE request failed: {}", e.kind)
        };
        log::error!("{message}");
        message.into()
    }

    /// A cache-keepalive ping (docs/transport.md): a **non-streaming** `max_tokens: 0`
    /// round-trip over the same prefix a real turn would send. It READS the cached
    /// prefix — refreshing its (sliding) TTL — and generates nothing, so the
    /// (empty) assistant turn is discarded and the log is never touched. Returns
    /// only the `usage`, which the daemon folds into the cache telemetry (docs/log-and-projection.md).
    ///
    /// Non-streaming on purpose: `max_tokens: 0` is rejected with `stream: true`
    /// (verified live). The prefix bytes must match a real request's **exactly**
    /// or the ping writes a *new* entry instead of refreshing the live one — so it
    /// takes the same `messages` / `tools` / `tool_choice` and reuses the client's
    /// `system`. (Set `tool_choice` to whatever the loop uses — `auto()`.)
    pub async fn keepalive(
        &self,
        messages: &[ApiMessage],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<Usage, Error> {
        let request = Request {
            model: &self.model,
            max_tokens: 0,
            system: &self.system,
            tools,
            tool_choice,
            thinking: THINKING_DISABLED,
            messages,
            stream: false,
        };
        let body = serde_json::to_string(&request)?;
        let mut conn = self
            .http
            .post(self.endpoint.url.as_str())
            .with_request_header(
                self.endpoint.auth_header.clone(),
                self.endpoint.auth_value.clone(),
            )
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json")
            .with_body(body)
            .await?;
        let ok = conn.status().is_some_and(|s| s.is_success());
        let text = conn.response_body().read_string().await?;
        if !ok {
            return Err(format!("keepalive returned non-2xx: {text}").into());
        }
        #[derive(Deserialize)]
        struct KeepaliveResponse {
            usage: Usage,
        }
        let parsed: KeepaliveResponse = serde_json::from_str(&text)?;
        Ok(parsed.usage)
    }
}
