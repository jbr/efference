//! The streaming client: `complete` and the cache `keepalive` (docs/transport.md).

use super::*;

use std::sync::Arc;
use std::time::Duration;

use trillium::Status;
use trillium_client::ClientHandler;
use trillium_client_retry::RetryHandler;

use crate::api::assembler::ResponseMeta;
use crate::log::is_captured_header;

/// Retry policy for requests that did not receive a successful response
/// (docs/transport.md). Every upstream request is a replay-safe POST carrying
/// an owned `String` body. The elapsed ceiling bounds an outage from the
/// operator's perspective; exhaustion still surfaces the final response.
const RETRY_MAX_ATTEMPTS: u32 = 4;
const RETRY_MAX_ELAPSED: Duration = Duration::from_secs(30);

/// How long one SSE response may stay open before the read loop stops waiting on
/// it (docs/transport.md). The retry policy above bounds *failures*; this bounds
/// *success taking forever* — a stream that neither finalizes nor dies has no
/// other bound, and blocks its turn with nothing on `GET /state` to read.
///
/// One hour. The widest model call in this repo's corpus (2026-08-28: 2281
/// assistant entries across every `.efference/*/log.jsonl`) took 363 seconds, and
/// the 99.9th percentile 253 — so an hour is roughly ten times the longest turn
/// anyone here has actually waited out. It is set that far above the observed
/// spread deliberately: hours-scale thinking is a thing models are moving toward,
/// and a ceiling that fires on a *slow* stream throws away real work, where one
/// that fires late only delays the diagnosis of a hang. Operators whose workload
/// says otherwise set `[transport] stream-ceiling` (`config::TransportLimits`).
pub const DEFAULT_STREAM_CEILING: Duration = Duration::from_secs(3600);

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

/// The all-clear: a 2xx means the upstream is answering, so the chip stops
/// reporting a failure that is over (docs/transport.md). This is the non-retry
/// half of the boundary — a retry that finalizes arrives here too, because
/// finalizing required a 2xx.
pub(super) fn clear_upstream_failure(failure: &UpstreamFailureState) {
    *failure.lock().unwrap() = None;
}

/// A non-2xx response body, read in the reverse pass and stashed on the conn.
///
/// Reading a body consumes it, and the caller that would otherwise read it — the
/// `into_sse` open error, or `keepalive` — runs *after* this handler. Parking the
/// bytes in the conn's state is what lets both the durable per-attempt record and
/// the caller's own error message quote the upstream, instead of one of them
/// finding an empty body.
struct CapturedErrorBody(String);

/// The stashed non-2xx body, if this conn's response carried one.
pub(super) fn captured_error_body(conn: &mut Conn) -> Option<String> {
    conn.take_state::<CapturedErrorBody>().map(|b| b.0)
}

/// Observes every upstream response in the reverse pass, before the outer retry
/// handler queues another attempt (docs/transport.md).
///
/// Three things happen here, all of them below the agent loop:
///
/// - a non-2xx updates the status/count diagnostic; a 2xx clears it, which is the
///   chip's all-clear boundary;
/// - a non-2xx body is read once and stashed, both for the caller and for the record;
/// - the failed attempt is queued for the loop to drain into a durable record-only
///   log entry — the handler cannot append, so the body crosses that boundary
///   through the queue.
struct UpstreamStatusHandler {
    failure: UpstreamFailureState,
    attempts: UpstreamAttemptQueue,
}

impl ClientHandler for UpstreamStatusHandler {
    async fn after_response(&self, conn: &mut Conn) -> trillium_client::Result<()> {
        let Some(status) = conn.status() else {
            return Ok(());
        };
        if status.is_success() {
            clear_upstream_failure(&self.failure);
            return Ok(());
        }
        let status = u16::from(status);
        record_upstream_failure(&self.failure, status);
        let body = conn
            .response_body()
            .read_string()
            .await
            .unwrap_or_else(|read_err| format!("(could not read error body: {read_err})"));
        conn.insert_state(CapturedErrorBody(body.clone()));
        push_upstream_attempt(&self.attempts, UpstreamAttemptFailure { status, body });
        Ok(())
    }
}

/// The status and allowlisted response headers of an executed conn
/// (docs/transport.md).
///
/// The allowlist is [`is_captured_header`]: a response header can carry
/// account-scoped values, so what may reach the log is a named set rather than
/// whatever the edge attached (docs/env.md). Values still pass the `[env]` masker
/// at append, like every other stored string.
pub(super) fn response_meta(conn: &Conn) -> ResponseMeta {
    let headers = conn
        .response_headers()
        .into_iter()
        .filter_map(|(name, values)| {
            let name = name.to_string();
            let value = values.one()?.as_str()?;
            is_captured_header(&name).then(|| (name.to_ascii_lowercase(), value.to_string()))
        })
        .collect();
    ResponseMeta {
        status: conn.status().map_or(0, u16::from),
        headers,
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
    /// How long one response's SSE stream may stay open (docs/transport.md).
    /// Session-constant and transport-only — it never reaches the request body,
    /// so it cannot move the cached prefix. Both stream surfaces ride it: the
    /// turn loop and the fork peek call the same [`Self::complete`].
    stream_ceiling: Duration,
    /// Which upstream serves this session, when pinned (docs/transport.md).
    upstream: Option<Upstream>,
    upstream_failure: UpstreamFailureState,
    /// A retry in flight, written by both retry loops (docs/transport.md). It
    /// rides on the client because the fork peek retries with a `Client` and no
    /// `Shared` in scope.
    upstream_retry: UpstreamRetryState,
    /// Failed HTTP attempts the retry handler absorbed, waiting for the agent
    /// loop to drain them into record-only log entries (docs/transport.md).
    upstream_attempts: UpstreamAttemptQueue,
}

impl Client {
    /// The provider-derived image placement this session's projections must use
    /// (docs/log-and-projection.md).
    pub fn image_placement(&self) -> crate::project::ImagePlacement {
        self.endpoint.provider.image_placement()
    }

    /// The provider-derived cache-breakpoint placement this session must use
    /// (docs/log-and-projection.md).
    pub fn breakpoint_placement(&self) -> crate::project::BreakpointPlacement {
        self.endpoint.provider.breakpoint_placement()
    }

    pub fn new(
        endpoint: Endpoint,
        model: impl Into<String>,
        system: impl Into<String>,
        temperature: Option<u8>,
    ) -> Self {
        let upstream_failure = upstream_failure_state();
        let upstream_attempts = upstream_attempt_queue();
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
                    attempts: Arc::clone(&upstream_attempts),
                },
            )),
            endpoint,
            model: model.into(),
            system: system.into(),
            temperature,
            model_reasoning: false,
            max_tokens: MAX_TOKENS,
            stream_ceiling: DEFAULT_STREAM_CEILING,
            upstream: None,
            upstream_failure,
            upstream_retry: upstream_retry_state(),
            upstream_attempts,
        }
    }

    /// Clone the transport diagnostic cell the daemon exposes through `GET /state`.
    pub fn upstream_failure(&self) -> UpstreamFailureState {
        Arc::clone(&self.upstream_failure)
    }

    /// Clone the retry-in-flight cell (docs/transport.md). The daemon publishes it
    /// as a state topic; the turn loop and the fork peek write it.
    pub fn upstream_retry(&self) -> UpstreamRetryState {
        Arc::clone(&self.upstream_retry)
    }

    /// Take every failed HTTP attempt recorded since the last drain, oldest first.
    /// The agent loop appends each as a record-only log entry, which is what makes
    /// a transient failure's *record* non-transient (docs/transport.md).
    pub fn drain_upstream_attempts(&self) -> Vec<UpstreamAttemptFailure> {
        drain_upstream_attempts(&self.upstream_attempts)
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

    /// This session's sticky key as the request body's `prompt_cache_key`, when
    /// there is one *and* the provider reads the field (docs/transport.md). Same
    /// two-sided gate as [`Self::with_session_id`], and the same value — the body
    /// copy of what the header already carries.
    fn prompt_cache_key(&self) -> Option<&str> {
        self.endpoint
            .provider
            .prompt_cache_key_field()
            .then_some(self.endpoint.session_id.as_deref())
            .flatten()
    }

    /// Record the capability lookup's reasoning answer
    /// (`ModelCapabilities::supports_reasoning`) — an input to the thinking matrix.
    /// Pin which upstream serves this session (docs/transport.md). Applies to
    /// every request the client makes, so a session's spotter and fork peeks
    /// route the same way its turns do.
    pub fn with_upstream(mut self, upstream: Option<Upstream>) -> Self {
        self.upstream = upstream;
        self
    }

    pub fn with_model_reasoning(mut self, model_reasoning: bool) -> Self {
        self.model_reasoning = model_reasoning;
        self
    }

    /// Set how long one response's SSE stream may stay open before the read loop
    /// gives up on it (`[transport] stream-ceiling`, docs/transport.md). Applies
    /// to every request this client makes, so a session's turns and its fork
    /// peeks are bounded alike.
    pub fn with_stream_ceiling(mut self, ceiling: Duration) -> Self {
        self.stream_ceiling = ceiling;
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
            upstream: self.upstream.as_ref(),
            prompt_cache_key: self.prompt_cache_key(),
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
        // The response the stream opened on, read off the executed conn before the
        // first event. Only the empty-finalized path keeps it (docs/transport.md) —
        // that path's whole record is otherwise one prose string, and the edge's
        // `x-generation-id` is the join key every after-the-fact query needs.
        // The `UpstreamStatusHandler` below cannot do this: an empty stream is a
        // 200, so the handler takes its success early-return, and at that point
        // nothing yet knows the stream will arrive empty.
        assembler.observed_response(response_meta(events.conn()));
        // A mid-stream death — a transport error, an in-stream `error` event, an
        // unparseable event — stops the read but never discards the partial: the
        // failure is recorded on the assembler and `finish` folds it into the
        // completion's `upstream_failure`, alongside whatever content accumulated
        // (docs/transport.md). Only a failure to *open* the stream (above) is an
        // `Err` from this function.
        //
        // The duration ceiling (docs/transport.md). A stream that stays open and
        // never finalizes is bounded by nothing else — the retry policy above
        // applies only while opening a response — so the read loop carries an
        // absolute deadline. Exceeding it is marked exactly like a mid-stream
        // death: the partial is kept, `finish` folds the reason into the
        // completion's `upstream_failure`, and the caller's abort-retry budget
        // decides what happens next.
        let deadline = tokio::time::Instant::now() + self.stream_ceiling;
        while let Some(event) = match tokio::time::timeout_at(deadline, events.next()).await {
            Ok(event) => event,
            Err(_) => {
                let why = format!(
                    "upstream SSE stream stayed open past the {}s ceiling without \
                     finalizing ([transport] stream-ceiling)",
                    self.stream_ceiling.as_secs()
                );
                log::error!("{why}");
                assembler.fail(why);
                None
            }
        } {
            let event = match event {
                Ok(event) => event,
                Err(e) => {
                    let why = format!("upstream SSE stream failed mid-stream: {e}");
                    log::error!("{why}");
                    assembler.fail(why);
                    break;
                }
            };
            match assembler.apply(event.event_type(), event.data()) {
                Ok(Some(streamed)) => on_delta(streamed),
                Ok(None) => {}
                Err(e) => {
                    let why = e.to_string();
                    log::error!("{why}");
                    assembler.fail(why);
                    break;
                }
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
    /// Note: this still *propagates* — the agent loop treats an open failure as
    /// terminal for the trajectory, though it now records the message durably (an
    /// empty record-only assistant entry, docs/transport.md) rather than leaving it
    /// on stderr alone. Classifying transient statuses (429 / 529 overload, 5xx) as
    /// worth further loop-level retries beyond the RetryHandler's own remains a
    /// separate, deferred change. Mid-stream deaths are different: they assemble as
    /// record-only partials and the loop retries them (docs/transport.md).
    async fn sse_open_error(e: SseError) -> Error {
        // `Status` is `Copy`, so binding it doesn't move `e.kind` — `e` stays whole and
        // convertible into its `Conn`.
        let message = if let SseErrorKind::Status(status) = e.kind {
            let mut conn: Conn = e.into();
            // The observing handler already read this body in the reverse pass and
            // stashed it on the conn; reading again would find it consumed.
            let body = match captured_error_body(&mut conn) {
                Some(body) => body,
                None => conn
                    .response_body()
                    .read_string()
                    .await
                    .unwrap_or_else(|read_err| format!("(could not read error body: {read_err})")),
            };
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
            upstream: self.upstream.as_ref(),
            prompt_cache_key: self.prompt_cache_key(),
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
        if !ok {
            // Same stash as the SSE open error: the handler consumed the body already.
            let text = match captured_error_body(&mut conn) {
                Some(text) => text,
                None => conn.response_body().read_string().await?,
            };
            return Err(format!("keepalive returned non-2xx: {text}").into());
        }
        let text = conn.response_body().read_string().await?;
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
