//! The streaming client: `complete` and the cache `keepalive` (docs/transport.md).

use super::*;

use std::sync::Arc;
use std::time::Duration;

use trillium::Status;
use trillium_client::ClientHandler;
use trillium_client_retry::RetryHandler;

/// Retry policy for requests that did not receive a successful response
/// (docs/transport.md). Every upstream request is a replay-safe POST carrying
/// an owned `String` body. The elapsed ceiling bounds an outage from the
/// operator's perspective; exhaustion still surfaces the final response.
const RETRY_MAX_ATTEMPTS: u32 = 4;
const RETRY_MAX_ELAPSED: Duration = Duration::from_secs(30);

/// Record one non-success upstream status. Matching statuses increment the
/// diagnostic's count; a different one starts a new run (docs/transport.md).
pub(super) fn record_upstream_failure(failure: &UpstreamFailureState, status: u16) {
    let mut failure = failure.lock().unwrap();
    match failure.as_mut() {
        Some(previous) if previous.status == status => {
            previous.count = previous.count.saturating_add(1);
        }
        _ => {
            *failure = Some(UpstreamFailure { status, count: 1 });
        }
    }
}

/// Records non-success upstream responses before the outer retry handler queues
/// another attempt. The cell retains the most recent status and counts repeated
/// occurrences; it deliberately does not clear on success because recovery has
/// no single transport lifecycle boundary (docs/transport.md).
struct UpstreamStatusHandler {
    failure: UpstreamFailureState,
}

impl ClientHandler for UpstreamStatusHandler {
    async fn after_response(&self, conn: &mut Conn) -> trillium_client::Result<()> {
        if let Some(status) = conn.status().filter(|status| !status.is_success()) {
            record_upstream_failure(&self.failure, u16::from(status));
        }
        Ok(())
    }
}

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
    temperature: Option<u8>,
    /// The boot-time capability lookup's answer to "does this model take
    /// OpenRouter's normalized `reasoning`" (`ModelCapabilities::supports_reasoning`)
    /// — one input to the thinking matrix (`Thinking::for_request`). `false` when no
    /// lookup ran (the spotter's client), which only ever omits the field.
    model_reasoning: bool,
    /// The per-turn output budget. Defaults to the pre-reasoning-era 4096;
    /// [`Self::with_max_output`] raises it to the model's reported cap — a
    /// reasoning model's thinking bills against this budget, and a long think
    /// against 4096 terminates at the cap before any text or call arrives
    /// (observed live, 2026-08-21). Session-constant, so it never perturbs the
    /// cached prefix.
    max_tokens: u32,
    upstream_failure: UpstreamFailureState,
}

impl Client {
    pub fn new(
        endpoint: Endpoint,
        model: impl Into<String>,
        system: impl Into<String>,
        temperature: Option<u8>,
    ) -> Self {
        let upstream_failure = upstream_failure_state();
        let retry = RetryHandler::default()
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
            .with_max_retry_after(RETRY_MAX_ELAPSED);
        Self {
            // TLS over the tokio TCP connector; ALPN negotiates HTTP/2 (no h3
            // advertised by Anthropic/Cloudflare as of 2026-06). See main.rs.
            // Retry remains outermost: reverse response handling records a failed
            // attempt before retry schedules the next one (docs/transport.md).
            http: HttpClient::new(RustlsConfig::<ClientConfig>::default()).with_handler((
                retry,
                UpstreamStatusHandler {
                    failure: Arc::clone(&upstream_failure),
                },
            )),
            endpoint,
            model: model.into(),
            system: system.into(),
            temperature,
            model_reasoning: false,
            max_tokens: MAX_TOKENS,
            upstream_failure,
        }
    }

    /// Clone the transport diagnostic cell the daemon exposes through `GET /state`.
    pub fn upstream_failure(&self) -> UpstreamFailureState {
        Arc::clone(&self.upstream_failure)
    }

    /// The resolved endpoint this client posts to. The daemon reads it for
    /// provider-conditional telemetry (cost tracking, docs/transport.md).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Carry this session's sticky-routing key, when there is one *and* the provider
    /// reads one (docs/transport.md). Both halves are conditional: offline verbs have
    /// no conversation to pin, and Anthropic has no sticky-routing concept at all —
    /// gating on the provider rather than sending the header everywhere keeps the
    /// design off the assumption that an unrecognized header is ignored.
    fn with_session_id(&self, conn: trillium_client::Conn) -> trillium_client::Conn {
        match (
            self.endpoint.provider.session_header(),
            &self.endpoint.session_id,
        ) {
            (Some(header), Some(id)) => conn.with_request_header(header, id.clone()),
            _ => conn,
        }
    }

    /// Record the capability lookup's reasoning answer
    /// (`ModelCapabilities::supports_reasoning`) — an input to the thinking matrix.
    pub fn with_model_reasoning(mut self, model_reasoning: bool) -> Self {
        self.model_reasoning = model_reasoning;
        self
    }

    /// Raise the per-turn output budget to the model's reported cap
    /// (`ModelCapabilities::max_output_tokens`). Values past `u32::MAX` clamp.
    pub fn with_max_output(mut self, cap: u64) -> Self {
        self.max_tokens = u32::try_from(cap).unwrap_or(u32::MAX);
        self
    }

    /// One streaming round-trip: serialize `messages`, POST with `stream: true`,
    /// and fold the SSE event stream back into a [`Completion`]. Each live signal — prose,
    /// thinking, a tool call opening, its arguments arriving — is handed to `on_delta` as a
    /// [`Streamed`] the moment it arrives: the transport-only live sink (the daemon publishes it to    /// the control stream's `output` class). Pass a no-op `|_| {}` when nothing is listening (e.g. the fork peek, whose
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
            max_tokens: self.max_tokens,
            system: &self.system,
            tools,
            tool_choice,
            thinking: Thinking::for_request(&self.model, &tool_choice, self.model_reasoning),
            temperature: self
                .temperature
                .map(|t| self.endpoint.provider.map_temperature(t)),
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

        let conn = self
            .http
            .post(self.endpoint.url.as_str())
            .with_request_header(
                self.endpoint.auth_header.clone(),
                self.endpoint.auth_value.clone(),
            )
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json");
        let conn = self.with_session_id(conn);
        let mut events = match conn.with_body(body).into_sse().await {
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
    ) -> Result<RequestUsage, Error> {
        let request = Request {
            model: &self.model,
            max_tokens: 0,
            system: &self.system,
            tools,
            tool_choice,
            thinking: Thinking::for_keepalive(&self.model),
            temperature: self
                .temperature
                .map(|t| self.endpoint.provider.map_temperature(t)),
            messages,
            stream: false,
        };
        let body = serde_json::to_string(&request)?;
        let conn = self
            .http
            .post(self.endpoint.url.as_str())
            .with_request_header(
                self.endpoint.auth_header.clone(),
                self.endpoint.auth_value.clone(),
            )
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json");
        let conn = self.with_session_id(conn);
        let mut conn = conn.with_body(body).await?;
        let ok = conn.status().is_some_and(|s| s.is_success());
        let text = conn.response_body().read_string().await?;
        if !ok {
            return Err(format!("keepalive returned non-2xx: {text}").into());
        }
        #[derive(Deserialize)]
        struct KeepaliveResponse {
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            model: Option<String>,
            usage: Usage,
        }
        let parsed: KeepaliveResponse = serde_json::from_str(&text)?;
        Ok(RequestUsage {
            id: parsed.id,
            model: parsed.model,
            usage: parsed.usage,
        })
    }
}
