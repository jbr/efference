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
/// One hour — roughly ten times the widest model call this repo's own corpus has
/// recorded, and set that far out deliberately: a ceiling that fires on a merely
/// *slow* stream discards real work, where one that fires late only delays the
/// diagnosis of a hang. Operators whose workload says otherwise set
/// `[transport] stream-ceiling` (`config::TransportLimits`).
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

/// Why a keepalive ping failed (docs/transport.md). A distinct type from the
/// transport's boxed [`Error`] because the keepalive loop's retry decision is a
/// function of the HTTP status: a request the upstream rejects as malformed will
/// be rejected identically on the next tick, so the loop must be able to read the
/// status structurally rather than parse an error string.
#[derive(Debug)]
pub enum KeepaliveError {
    /// The upstream answered with a non-2xx. `status` is `None` when the response
    /// carried no status line at all; `body` is the upstream's text.
    Status { status: Option<u16>, body: String },
    /// Anything before or after the status: connection failure, unreadable body,
    /// a response that did not parse.
    Other(Error),
}

impl KeepaliveError {
    pub fn other(e: impl Into<Error>) -> Self {
        Self::Other(e.into())
    }

    /// Whether this failure will recur identically on the next ping — the upstream
    /// rejected the request itself, not the moment. True for the 4xx range except
    /// 408 (request timeout) and 429 (rate limit), which describe the moment. 401
    /// and 403 count as permanent here because the client's credential is fixed at
    /// boot, so a rejected credential stays rejected for the life of the session.
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Status {
                status: Some(s), ..
            } => (400..500).contains(s) && *s != 408 && *s != 429,
            _ => false,
        }
    }
}

impl std::fmt::Display for KeepaliveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status {
                status: Some(status),
                body,
            } => write!(f, "keepalive returned {status}: {body}"),
            Self::Status { status: None, body } => {
                write!(f, "keepalive returned no status: {body}")
            }
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for KeepaliveError {}

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

/// A thin Anthropic Messages client, holding the resolved endpoint, the model,
/// and the session's system prompt. The system is byte-stable across the
/// session, so it stays in the cached prefix (docs/log-and-projection.md); the
/// fork (docs/redaction-fork.md) shares the same endpoint and system, so its
/// prefix matches and hits cache.
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
    /// The per-turn output cap. Defaults to the pre-reasoning-era 4096;
    /// [`Self::with_max_output`] raises it to the model's reported cap — a
    /// reasoning model's thinking bills against this budget, and a long think
    /// against 4096 terminates at the cap before any text or call arrives.
    /// What goes on the wire is this cap shrunk to what the window spares
    /// after the prompt (`api/reservation.rs`, docs/transport.md).
    max_tokens: u32,
    /// The model's context window, from the boot capability lookup
    /// ([`Self::with_context_window`]); `None` sends the cap unshrunk.
    context_length: Option<u64>,
    /// The reservation's calibration and last-sent value, shared with the
    /// daemon's `GET /state` view (docs/transport.md).
    reservation: ReservationState,
    /// How long one response's SSE stream may stay open (docs/transport.md).
    /// Session-constant and transport-only — it never reaches the request body,
    /// so it cannot move the cached prefix. Both stream surfaces ride it: the
    /// turn loop and the fork peek call the same [`Self::complete`].
    stream_ceiling: Duration,
    /// The session's effort level, when one is set (docs/transport.md). Rides
    /// the shared envelope only — [`Self::complete`], which both the turn loop
    /// and the fork peek call, so the peek's prefix still matches the agent
    /// lineage's (docs/redaction-fork.md). The keepalive deliberately omits it
    /// (`max_tokens: 0` sends thinking disabled), and the spotter's client never
    /// sets it.
    effort: Option<Effort>,
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
        self.endpoint.provider.image_placement(&self.model)
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
            // The process-wide client (`http.rs`), so every `Client` in a process
            // — the agent's, the fork's spotter, one per case in a spotter eval —
            // shares one pool to the provider. TLS over the tokio TCP connector;
            // ALPN negotiates HTTP/2 — Anthropic's Cloudflare front advertises no
            // `alt-svc: h3`, so HTTP/3 is not a live option (docs/transport.md).
            // Retry remains outermost: reverse response handling records a failed
            // attempt before retry schedules the next one (docs/transport.md).
            http: crate::http::client().with_handler((
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
            context_length: None,
            reservation: reservation_state(),
            stream_ceiling: DEFAULT_STREAM_CEILING,
            effort: None,
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

    /// Carry the provider's app-attribution headers, if it has any
    /// ([`Provider::attribution`], docs/transport.md). Constant per provider, and
    /// headers rather than body, so they sit outside the cached prefix.
    fn with_attribution(&self, conn: trillium_client::Conn) -> trillium_client::Conn {
        self.endpoint
            .provider
            .attribution()
            .iter()
            .fold(conn, |conn, (name, value)| {
                conn.with_request_header(*name, *value)
            })
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

    /// Pin which upstream serves this session (docs/transport.md). Applies to
    /// every request the client makes, so a session's spotter and fork peeks
    /// route the same way its turns do.
    pub fn with_upstream(mut self, upstream: Option<Upstream>) -> Self {
        self.upstream = upstream;
        self
    }

    /// Set the session's effort level (`--effort`, `[transport] effort` —
    /// docs/transport.md). Boot-time only: it is set before the first turn and
    /// never changed, since a mid-session change would move the cached prefix.
    /// Applies to every [`Self::complete`] this client makes, so the turn and
    /// the fork peek carry the same envelope; the keepalive is not built from it.
    pub fn with_effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    /// Record the capability lookup's reasoning answer
    /// (`ModelCapabilities::supports_reasoning`) — an input to the thinking matrix
    /// ([`Thinking::for_request`]).
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

    /// Set the model's context window (`ModelCapabilities::context_length`), so
    /// each request's `max_tokens` is shrunk to what the window spares after
    /// the prompt (`api/reservation.rs`). Unset, the cap is sent as is.
    pub fn with_context_window(mut self, context_length: u64) -> Self {
        self.context_length = Some(context_length);
        self
    }

    /// Clone the reservation cell the daemon reads for `GET /state`'s context
    /// limit (docs/transport.md).
    pub fn reservation(&self) -> ReservationState {
        Arc::clone(&self.reservation)
    }

    /// The `max_tokens` for a request whose variable parts serialize to
    /// `body_bytes` bytes (`api/reservation.rs`).
    fn reserve(&self, body_bytes: usize) -> u32 {
        let reserved = self.reservation.lock().unwrap().compute(
            u64::from(self.max_tokens),
            self.context_length,
            body_bytes,
        );
        u32::try_from(reserved).unwrap_or(u32::MAX)
    }

    /// One streaming round-trip: serialize `messages`, POST with `stream: true`,
    /// and fold the SSE event stream back into a [`Completion`]. Each live signal
    /// — prose, thinking, a tool call opening, its arguments arriving — is handed
    /// to `on_delta` as a [`Streamed`] the moment it arrives: the transport-only
    /// live sink the daemon publishes to the control stream's `output` class.
    /// Pass a no-op `|_| {}` when nothing is listening (the fork peek, whose
    /// triage tokens must never surface).
    ///
    /// `into_sse` *is* the execution — the conn must not be awaited first (that
    /// is `SseErrorKind::AlreadyExecuted`). On a non-2xx it returns
    /// `SseErrorKind::Status` carrying the code; the failed `SseError` still owns the
    /// `Conn` (`SseError: Into<Conn>`), so [`Self::sse_open_failure`] reads the response
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
        // The prompt's variable parts, sized before `max_tokens` is chosen: the
        // reservation is a function of how much of the window the prompt takes
        // (`api/reservation.rs`), estimated from these bytes against the last
        // response's exact count.
        let body_bytes = self.system.len()
            + serde_json::to_vec(messages)?.len()
            + serde_json::to_vec(tools)?.len();
        let mut max_tokens = self.reserve(body_bytes);
        let mut events = loop {
            let request = Request {
                model: &self.model,
                max_tokens,
                system: &self.system,
                tools,
                tool_choice,
                thinking: Thinking::for_request(&self.model, &tool_choice, self.model_reasoning),
                output_config: self.effort.map(|effort| OutputConfig { effort }),
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
                "POST {}: {} messages, {} request bytes, max_tokens {max_tokens}",
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
            let conn = self.with_attribution(self.with_session_id(conn));
            match conn.with_body(body).into_sse().await {
                Ok(events) => break events,
                Err(e) => {
                    let (status, detail) = Self::sse_open_failure(e).await;
                    // The window backstop (`api/reservation.rs`): the estimate
                    // undershot and the provider named the overrun, so halve
                    // the reservation and go again — once per halving, down to
                    // the floor. Any other open failure stays terminal.
                    if status == Some(Status::BadRequest) && is_window_overflow(&detail) {
                        if let Some(halved) = self.reservation.lock().unwrap().halve() {
                            log::warn!(
                                "prompt plus max_tokens {max_tokens} overran the window; \
                                 retrying with max_tokens {halved}"
                            );
                            max_tokens = u32::try_from(halved).unwrap_or(u32::MAX);
                            continue;
                        }
                    }
                    return Err(Self::open_error(status, detail));
                }
            }
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
        let completion = assembler.finish();
        // Anchor the next estimate to this response's exact prompt count
        // (`api/reservation.rs`). An attempt that died with zero usage
        // counted nothing and teaches nothing.
        if let Ok(completion) = &completion {
            let prompt_tokens = completion.usage.input_tokens
                + completion.usage.cache_read_input_tokens
                + completion.usage.cache_creation_input_tokens;
            if prompt_tokens > 0 {
                self.reservation
                    .lock()
                    .unwrap()
                    .calibrate(prompt_tokens, body_bytes);
            }
        }
        completion
    }

    /// Turn a failed `into_sse` into a legible [`Error`], reading the response body on a
    /// non-2xx. The `SseError` still owns the `Conn` (it `Deref`s to it and converts
    /// back via `Into<Conn>`), so on `Status` we read the body — Anthropic's error
    /// responses carry a JSON `error.message` that names the actual problem. Logged at
    /// `error` so the daemon's log preserves it even though the error also propagates.
    ///
    /// This *propagates*: the agent loop treats an open failure as terminal for
    /// the trajectory, recording the message durably as an empty record-only
    /// assistant entry. Mid-stream deaths are the different case — they assemble
    /// as record-only partials and the loop retries them (docs/transport.md).
    async fn sse_open_failure(e: SseError) -> (Option<Status>, String) {
        // `Status` is `Copy`, so binding it doesn't move `e.kind` — `e` stays whole and
        // convertible into its `Conn`.
        if let SseErrorKind::Status(status) = e.kind {
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
            (Some(status), body)
        } else {
            (None, e.kind.to_string())
        }
    }

    /// The [`Error`] for an open failure [`Self::sse_open_failure`] took apart:
    /// the status and body on a non-2xx, the transport's own text otherwise.
    fn open_error(status: Option<Status>, detail: String) -> Error {
        let message = match status {
            Some(status) => format!("upstream API returned {status}: {detail}"),
            None => format!("upstream SSE request failed: {detail}"),
        };
        log::error!("{message}");
        message.into()
    }

    /// A cache-keepalive ping (docs/transport.md): a **non-streaming**
    /// `max_tokens: 0` round-trip over the same prefix a real turn would send.
    /// Returns only the `usage`; the log is never touched.
    ///
    /// Non-streaming because `max_tokens: 0` is rejected with `stream: true`. The
    /// prefix bytes must match a real request's **exactly** or the ping writes a
    /// *new* cache entry instead of refreshing the live one — which is why it
    /// takes the caller's `messages` / `tools` / `tool_choice` (the loop's
    /// `auto()`) and reuses the client's own `system`, rather than reconstructing
    /// any of them.
    pub async fn keepalive(
        &self,
        messages: &[ApiMessage],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<RequestUsage, KeepaliveError> {
        let request = Request {
            model: &self.model,
            max_tokens: 0,
            system: &self.system,
            tools,
            tool_choice,
            thinking: Thinking::for_keepalive(&self.model),
            // No effort: this ping sends thinking disabled/omitted (`max_tokens: 0`
            // rejects thinking-on), and `xhigh`/`max` with thinking off is a 400.
            // Its envelope therefore differs from a turn's by this field whenever a
            // session sets one — the watch-item in docs/transport.md.
            output_config: None,
            temperature: self
                .temperature
                .map(|t| self.endpoint.provider.map_temperature(t)),
            upstream: self.upstream.as_ref(),
            prompt_cache_key: self.prompt_cache_key(),
            messages,
            stream: false,
        };
        let body = serde_json::to_string(&request).map_err(KeepaliveError::other)?;
        let conn = self
            .http
            .post(self.endpoint.url.as_str())
            .with_request_header(
                self.endpoint.auth_header.clone(),
                self.endpoint.auth_value.clone(),
            )
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json");
        let conn = self.with_attribution(self.with_session_id(conn));
        let mut conn = conn.with_body(body).await.map_err(KeepaliveError::other)?;
        let status = conn.status().map(|s| s as u16);
        if !status.is_some_and(|s| (200..300).contains(&s)) {
            // Same stash as the SSE open error: the handler consumed the body already.
            let body = match captured_error_body(&mut conn) {
                Some(text) => text,
                None => conn
                    .response_body()
                    .read_string()
                    .await
                    .map_err(KeepaliveError::other)?,
            };
            return Err(KeepaliveError::Status { status, body });
        }
        let text = conn
            .response_body()
            .read_string()
            .await
            .map_err(KeepaliveError::other)?;
        #[derive(Deserialize)]
        struct KeepaliveResponse {
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            model: Option<String>,
            usage: Usage,
        }
        let parsed: KeepaliveResponse =
            serde_json::from_str(&text).map_err(KeepaliveError::other)?;
        Ok(RequestUsage {
            id: parsed.id,
            model: parsed.model,
            usage: parsed.usage,
        })
    }
}
