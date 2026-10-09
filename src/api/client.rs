//! The streaming client: `complete` and the cache `keepalive` (docs/transport.md).

use super::*;

use std::sync::Arc;
use std::time::Duration;

use trillium::Status;
use trillium_client::ClientHandler;
use trillium_client_retry::RetryHandler;

use crate::api::assembler::ResponseMeta;
use crate::api::auth_value::Reloaded;
use crate::api::failure::{FailureClass, OpenError, classify_body};
use crate::api::plan::{PlanState, plan_from_headers, plan_state};
use crate::api::responses::assembler::ResponsesAssembler;
use crate::api::stages::{ActiveTrace, StageSink, Trace};
use crate::api::thinking::ThinkingSupport;
use crate::log::{RequestOrigin, is_captured_header};
use crate::project::{WireTurn, encode_messages};

mod images;

/// Retry policy for requests that did not receive a successful response
/// (docs/transport.md). Every upstream request is a replay-safe POST carrying
/// an owned `String` body. Bounded by attempt count alone: there is no elapsed
/// budget, because the handler enforces one by clamping each retry's timeout to
/// what remains of it, and a model's time to first byte on a large uncached
/// prompt can be most of that budget by itself — a retry cut short by the clamp
/// fails for the clamp, not for the upstream. Exhaustion still surfaces the
/// final response, and the loops' own schedules bound an outage beyond that.
const RETRY_MAX_ATTEMPTS: u32 = 4;
/// The longest `Retry-After` the transport waits out in place; a longer one
/// surfaces as the failed attempt, for the loop's schedule to absorb.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

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

/// The non-2xx statuses the transport retries on its own, before the agent loop
/// sees a failure (docs/transport.md): the rate-limit and server-side family.
const RETRY_STATUSES: [Status; 5] = [
    Status::TooManyRequests,
    Status::InternalServerError,
    Status::BadGateway,
    Status::ServiceUnavailable,
    Status::GatewayTimeout,
];

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
    /// The turn's sticky-routing token and the header it arrives on
    /// (docs/transport.md); `None` where the provider has no such contract.
    turn_routing: Option<(&'static str, TurnRoutingState)>,
    /// The subscription-window snapshot (`api/plan.rs`), on the provider that
    /// reports one. Read on every status: the 429 that ends a window carries it.
    plan: Option<PlanState>,
    /// The request whose attempt this is, when stages are recorded
    /// (`api/stages.rs`).
    active: ActiveTrace,
}

impl ClientHandler for UpstreamStatusHandler {
    async fn after_response(&self, conn: &mut Conn) -> trillium_client::Result<()> {
        let Some(status) = conn.status() else {
            return Ok(());
        };
        if let Some(cell) = &self.plan {
            let headers = conn.response_headers();
            let observed_at = crate::log::Timestamp::now().0;
            let names: Vec<String> = headers
                .iter()
                .map(|(name, _)| name.as_ref().to_string())
                .collect();
            if let Some(snapshot) = plan_from_headers(
                names,
                |name| headers.get_str(name).map(str::to_string),
                observed_at,
            ) {
                *cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
            }
        }
        if status.is_success() {
            clear_upstream_failure(&self.failure);
            // First token of the turn wins: the backend issues it at turn start and
            // the contract is to replay that value unchanged until the turn ends.
            if let Some((header, cell)) = &self.turn_routing
                && let Some(token) = conn.response_headers().get_str(*header)
            {
                let mut slot = cell.lock().unwrap_or_else(|e| e.into_inner());
                if slot.is_none() {
                    *slot = Some(token.to_string());
                }
            }
            return Ok(());
        }
        let status = u16::from(status);
        record_upstream_failure(&self.failure, status);
        let active = self.active.lock().unwrap().clone();
        if let Some(active) = active {
            active.attempt_failed(status);
        }
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
///
/// `Clone` is a cheap hand-off of *the same* client: the Arc-backed live states
/// (reservation, upstream retry and failure) are shared, so a clone observes and
/// joins the same retry story the original does.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    endpoint: Endpoint,
    model: String,
    system: String,
    temperature: Option<u8>,
    /// The capability lookup's answer to how this model takes `thinking`
    /// (`ModelCapabilities::thinking`) — the input to `Thinking::for_request`, and
    /// on Responses whether the `reasoning` object is sent at all.
    /// [`ThinkingSupport::UNKNOWN`] until [`Self::with_thinking`] sets it.
    thinking: ThinkingSupport,
    /// Never think, whatever the model would do on its own (docs/redaction-fork.md):
    /// the separate-model fork peek's default, where the peek's reasoning is
    /// discarded with the tail and costs latency and tokens on every gate.
    pub(super) thinking_off: bool,
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
    /// The operator's interrupt, when this client's reads answer to one
    /// (docs/control-channel.md): `POST /loop/interrupt` fires it, and a read in
    /// flight ends there — the stream is dropped, which is what frees a hung
    /// socket, and the partial comes back marked [`FailureClass::Aborted`].
    /// `None` for clients nothing interrupts (the spotter, the dev verbs).
    abort: Option<Arc<tokio::sync::Notify>>,
    /// The session's effort level, when one is set (docs/transport.md). Rides
    /// every request this client makes — the turn loop and the fork peek, so the
    /// peek's prefix still matches the agent lineage's (docs/redaction-fork.md),
    /// and the keepalive, which sends the turn's envelope. The spotter's client
    /// never sets it.
    effort: Option<String>,
    /// Which upstream serves this session, when pinned (docs/transport.md).
    upstream: Option<Upstream>,
    /// Who places cache-write boundaries for the selected model (docs/transport.md).
    /// Read by the Responses request: explicit policy sends
    /// `prompt_cache_options.mode: explicit` and the trailing marker; anything else
    /// sends neither. Messages ignores it — its marker is unconditional.
    prompt_cache: PromptCachePolicy,
    upstream_failure: UpstreamFailureState,
    /// Set once the model has answered a forced `tool_choice` with a request
    /// rejection ([`is_request_rejection`])
    /// (docs/transport.md): every later forced request from this client, and its
    /// clones, goes as `auto` with [`forced_choice_instruction`] instead. Shared
    /// across clones, so the lesson is learned once per client.
    forced_choice_refused: Arc<std::sync::atomic::AtomicBool>,
    /// The endpoint's refusal, set once it refused a request carrying images and
    /// the same request without them succeeded (docs/transport.md): every later
    /// request from this client and its clones goes with its images replaced by a
    /// placeholder.
    images_refused: Arc<std::sync::OnceLock<String>>,
    /// Set once a request carrying images has succeeded, which ends the recovery
    /// above: the model reads images, so a later rejection has another cause.
    images_accepted: Arc<std::sync::atomic::AtomicBool>,
    /// A retry in flight, written by both retry loops (docs/transport.md). It
    /// rides on the client because the fork peek retries with a `Client` and no
    /// `Session` in scope.
    upstream_retry: UpstreamRetryState,
    /// Failed HTTP attempts the retry handler absorbed, waiting for the agent
    /// loop to drain them into record-only log entries (docs/transport.md).
    upstream_attempts: UpstreamAttemptQueue,
    /// The sticky-routing token for the user turn in flight, shared with the
    /// response handler that fills it (docs/transport.md). `None` where the
    /// provider has no such contract, or on a client whose every request is its
    /// own conversation (the spotter's), which must not replay one.
    turn_routing: Option<TurnRoutingState>,
    /// The latest subscription-window snapshot (`api/plan.rs`), shared with the
    /// response handler that fills it and read by the daemon's `usage` topic.
    /// Allocated for every client; only the Codex arm's handler ever writes it.
    plan: PlanState,
    /// Where this client reports each request's stages, installed by the session
    /// (`set_stage_sink`; docs/log-and-projection.md). `None` records nothing.
    stages: Option<StageSink>,
    /// What the caller says these requests are for, tagged on each recorded stage
    /// ([`Self::with_origin`]). [`RequestOrigin::Unknown`] until a caller says.
    origin: RequestOrigin,
    /// The request in flight, for the retry handler (`api/stages.rs`).
    active_trace: ActiveTrace,
}

/// The user line a forced call sent as `auto` ends with (docs/transport.md).
pub(crate) fn forced_choice_instruction(tool: &str) -> String {
    format!("Answer by calling the `{tool}` tool, and only that tool.")
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
        self.endpoint.provider.breakpoint_placement(&self.model)
    }

    pub fn new(
        endpoint: Endpoint,
        model: impl Into<String>,
        system: impl Into<String>,
        temperature: Option<u8>,
    ) -> Self {
        let upstream_failure = upstream_failure_state();
        let upstream_attempts = upstream_attempt_queue();
        let turn_routing = endpoint
            .provider
            .turn_routing_header()
            .map(|_| turn_routing_state());
        let plan = plan_state();
        let active_trace = ActiveTrace::default();
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
            // The status set above, with one veto: a body that names a condition
            // no retry can change (`api/failure.rs`) — the Codex backend answers
            // an exhausted plan with a 429 whose body says `usage_limit_reached`
            // and when it resets — fails at once instead of spending the
            // schedule on the same answer. The status handler runs first in the
            // reverse pass and stashes the body, which is what this reads.
            .retry_when(|conn| {
                // No status means no response: a transport error, retried as the
                // default gate does.
                let Some(status) = conn.status() else {
                    return true;
                };
                if !RETRY_STATUSES.contains(&status) {
                    return false;
                }
                conn.state::<CapturedErrorBody>()
                    .is_none_or(|body| classify_body(&body.0) == FailureClass::Transient)
            })
            .with_max_attempts(RETRY_MAX_ATTEMPTS)
            .with_max_elapsed(None)
            .with_max_retry_after(RETRY_AFTER_CAP);
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
                    turn_routing: endpoint
                        .provider
                        .turn_routing_header()
                        .zip(turn_routing.as_ref().map(Arc::clone)),
                    plan: endpoint
                        .provider
                        .reports_plan_windows()
                        .then(|| Arc::clone(&plan)),
                    active: Arc::clone(&active_trace),
                },
            )),
            endpoint,
            model: model.into(),
            system: system.into(),
            temperature,
            thinking: ThinkingSupport::UNKNOWN,
            thinking_off: false,
            max_tokens: MAX_TOKENS,
            context_length: None,
            reservation: reservation_state(),
            stream_ceiling: DEFAULT_STREAM_CEILING,
            abort: None,
            effort: None,
            upstream: None,
            prompt_cache: PromptCachePolicy::None,
            upstream_failure,
            upstream_retry: upstream_retry_state(),
            forced_choice_refused: Arc::default(),
            images_refused: Arc::default(),
            images_accepted: Arc::default(),
            upstream_attempts,
            turn_routing,
            plan,
            stages: None,
            origin: RequestOrigin::Unknown,
            active_trace,
        }
    }

    /// Install the sink this client's requests report their stages to
    /// (docs/log-and-projection.md). Clones made afterwards share it.
    pub fn set_stage_sink(&mut self, sink: StageSink) {
        self.stages = Some(sink);
    }

    /// This client, tagging the requests it sends with `origin`. A cheap clone: the
    /// shared cells (retry, reservation, the sink) stay shared.
    pub fn with_origin(&self, origin: RequestOrigin) -> Self {
        Self {
            origin,
            ..self.clone()
        }
    }

    /// Clone the subscription-window cell the daemon publishes as `usage.plan`
    /// (docs/transport.md).
    pub fn plan_state(&self) -> PlanState {
        Arc::clone(&self.plan)
    }

    /// A client whose every request is its own conversation (the spotter's): it
    /// neither keeps nor replays a turn-routing token, since the token belongs
    /// to one user turn and this client is never inside one (docs/transport.md).
    pub fn without_turn_routing(mut self) -> Self {
        self.turn_routing = None;
        self
    }

    /// A user turn is starting (docs/transport.md): forget the previous turn's
    /// routing token so the first request of this one goes out without a header
    /// and the backend issues a fresh token. Called by the agent loop at each
    /// trajectory start and each seam-drained user turn; the fork peek shares this
    /// client and so the token, which is the point — the peek is a request of the
    /// same turn.
    pub fn begin_turn(&self) {
        if let Some(cell) = &self.turn_routing {
            *cell.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// The turn-routing token in hand, for tests and the daemon's view.
    pub fn turn_routing_token(&self) -> Option<String> {
        self.turn_routing
            .as_ref()
            .and_then(|cell| cell.lock().unwrap_or_else(|e| e.into_inner()).clone())
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
    pub fn with_prompt_cache(mut self, policy: PromptCachePolicy) -> Self {
        self.prompt_cache = policy;
        self
    }

    /// The model this client sends every request to.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The wire dialect this client speaks, fixed by the provider (docs/transport.md).
    pub fn dialect(&self) -> WireDialect {
        self.endpoint
            .provider
            .inference_dialect()
            .expect("a client is only built on a provider that serves turns")
    }

    pub fn with_upstream(mut self, upstream: Option<Upstream>) -> Self {
        self.upstream = upstream;
        self
    }

    /// Set the session's effort level (`--effort`, `[transport] effort` —
    /// docs/transport.md). Boot-time only: it is set before the first turn and
    /// never changed, since a mid-session change would move the cached prefix.
    /// Applies to every [`Self::complete`] this client makes, so the turn and
    /// the fork peek carry the same envelope; the keepalive is not built from it.
    pub fn with_effort(mut self, effort: Option<String>) -> Self {
        self.effort = effort;
        self
    }

    /// Record the capability lookup's thinking answer
    /// (`ModelCapabilities::thinking`), which [`Thinking::for_request`] reads.
    pub fn with_thinking(mut self, thinking: ThinkingSupport) -> Self {
        self.thinking = thinking;
        self
    }

    /// The `reasoning` field a request carries: OpenRouter's switch, off, and only
    /// where `thinking` says nothing — a foreign model on OpenRouter gets that
    /// field omitted, so the switch this client asked for has to be said in
    /// OpenRouter's own word. `None` everywhere else, which serializes nothing.
    pub(super) fn reasoning_off(&self, thinking: Thinking) -> Option<ReasoningOff> {
        (self.thinking_off && thinking.is_omit() && self.endpoint.provider == Provider::OpenRouter)
            .then_some(ReasoningOff { enabled: false })
    }

    /// Send every request with thinking off, whatever the model would do on its
    /// own (docs/redaction-fork.md): the separate-model fork peek's default. Spelled
    /// the way the model's metadata says ([`ThinkingSupport::off`]), so set the
    /// capability answer with [`Self::with_thinking`] as well.
    pub fn without_thinking(mut self) -> Self {
        self.thinking_off = true;
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

    /// Make this client's reads answer to the operator's interrupt
    /// (docs/control-channel.md). Set by the runtime once the session exists,
    /// on the agent client and the own-triage client alike.
    pub fn set_abort(&mut self, abort: Arc<tokio::sync::Notify>) {
        self.abort = Some(abort);
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
    pub fn with_context_window(mut self, context_length: Option<u64>) -> Self {
        self.context_length = context_length;
        self
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
    ///
    /// A forced `tool_choice` the model rejects ([`is_request_rejection`]) is
    /// retried once as
    /// `auto`, with a user line naming the tool and thinking kept off as the forced
    /// call had it (docs/transport.md). Some models refuse forced choice outright
    /// and their metadata does not say so; the retry is how the client learns it.
    /// When the retry succeeds, the client remembers, and later forced requests go
    /// straight to the fallback. When it fails too, the rejection had another cause: the
    /// first error is returned and nothing is learned.
    pub async fn complete(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        use std::sync::atomic::Ordering;
        let Some(forced) = tool_choice.forced() else {
            return self.send(turns, tools, tool_choice, None, on_delta).await;
        };
        if self.forced_choice_refused.load(Ordering::Relaxed) {
            return self
                .send_unforced(turns, tools, forced, &mut on_delta)
                .await;
        }
        let refusal = match self
            .send(turns, tools, tool_choice, None, &mut on_delta)
            .await
        {
            Err(e) if is_request_rejection(&e) => e,
            answered => return answered,
        };
        match self
            .send_unforced(turns, tools, forced, &mut on_delta)
            .await
        {
            Ok(completion) => {
                log::warn!(
                    "{} refused a forced tool_choice ({refusal}); this client sends `auto` \
                     with an instruction from here on",
                    self.model
                );
                self.forced_choice_refused.store(true, Ordering::Relaxed);
                Ok(completion)
            }
            Err(_) => Err(refusal),
        }
    }

    /// A forced call sent as `auto`: the turns plus [`forced_choice_instruction`],
    /// with thinking off, which is what the forced request would have sent.
    async fn send_unforced(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        forced: &str,
        on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        let mut turns = turns.to_vec();
        turns.push(WireTurn::text(forced_choice_instruction(forced)));
        self.clone()
            .without_thinking()
            .send(&turns, tools, ToolChoice::auto(), None, on_delta)
            .await
    }

    /// [`Self::complete`], with the output cap fixed at `cap` instead of reserved
    /// against the window when one is given. A fixed cap is sent as-is, also on the
    /// Codex profile that otherwise omits it, and a refusal of it is not retried at
    /// another value: the caller chose it (the Responses keepalive, docs/transport.md).
    /// Reached through [`Self::send`], which handles an image refusal.
    async fn send_request(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        cap: Option<u32>,
        on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        let trace = Trace::begin(self.stages.as_ref(), self.origin, &self.active_trace);
        let result = self
            .send_traced(trace.as_ref(), turns, tools, tool_choice, cap, on_delta)
            .await;
        if let Some(trace) = trace {
            match &result {
                Err(e) => trace.failed(&e.to_string()),
                Ok(completion) if completion.operator_abort() => trace.interrupted(),
                Ok(completion) => match completion.upstream_failure() {
                    Some(why) => trace.failed(why),
                    None => trace.ended(completion.finish.as_ref().map(|f| f.raw.as_str())),
                },
            }
        }
        result
    }

    /// [`Self::send`]'s request itself, reporting the stages it passes to `trace`.
    async fn send_traced(
        &self,
        trace: Option<&Trace>,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        cap: Option<u32>,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        // A turn list recorded in the other dialect never becomes a request: the
        // encoder would skip every assistant turn and send a history with the
        // model's replies missing (docs/transport.md). Live sessions are pinned to
        // one provider, so this answers only a recorded session continued or
        // rendered under another.
        crate::project::check_dialect(turns, self.dialect())?;
        // Encode once per dialect (docs/transport.md). The encoded form is what the
        // reservation sizes and what each retry re-sends verbatim.
        let encoded = match self.dialect() {
            WireDialect::Messages => Encoded::Messages(encode_messages(
                turns,
                self.image_placement(),
                self.breakpoint_placement(),
            )),
            WireDialect::Responses => {
                let preamble = match self.endpoint.provider.responses_profile() {
                    ResponsesProfile::Platform => self.system.as_str(),
                    // Rides `instructions` instead (docs/transport.md).
                    ResponsesProfile::Codex => "",
                };
                Encoded::Responses(encode_responses(turns, preamble, self.prompt_cache))
            }
            // No configuration names the dialect until its client lands
            // (`WireDialect::from_name`).
            WireDialect::Completions => unreachable!("the Completions dialect is not served yet"),
            WireDialect::SystemOne => {
                unreachable!("a client is only built on a provider that serves turns")
            }
        };
        // The prompt's variable parts, sized before `max_tokens` is chosen: the
        // reservation is a function of how much of the window the prompt takes
        // (`api/reservation.rs`), estimated from these bytes against the last
        // response's exact count.
        let body_bytes = self.system.len()
            + match &encoded {
                Encoded::Messages(messages) => serde_json::to_vec(messages)?.len(),
                Encoded::Responses(input) => serde_json::to_vec(input)?.len(),
            }
            + serde_json::to_vec(tools)?.len();
        let mut max_tokens = cap.unwrap_or_else(|| self.reserve(body_bytes));
        // The credential re-read below happens at most once per call: a source
        // that yields a working token yields it the first time, and a second
        // pass would only re-read the same bytes.
        let mut reloaded_auth = false;
        // `Sent` is reported once per request; a reservation or credential retry
        // loops back through the open below.
        let mut sent_reported = false;
        // The operator's interrupt (docs/control-channel.md) is armed before the
        // request is sent, not once its stream opens: `notify_waiters` stores no
        // permit, so a press that lands after the request reaches the upstream but
        // before the read loop below registers would otherwise be lost, leaving the
        // read on a stream that may never finish. Armed here, it is ready the first
        // time the read loop polls it.
        let abort_signal = self
            .abort
            .clone()
            .unwrap_or_else(|| Arc::new(tokio::sync::Notify::new()));
        let abort = abort_signal.notified();
        tokio::pin!(abort);
        abort.as_mut().enable();
        let mut events = loop {
            let body = match &encoded {
                Encoded::Messages(messages) => {
                    let thinking =
                        Thinking::for_request(&tool_choice, self.thinking, self.thinking_off);
                    let request = Request {
                        model: &self.model,
                        max_tokens,
                        system: &self.system,
                        tools,
                        tool_choice,
                        thinking,
                        reasoning: self.reasoning_off(thinking),
                        output_config: self.effort.clone().map(|effort| OutputConfig { effort }),
                        temperature: self
                            .temperature
                            .map(|t| self.endpoint.provider.map_temperature(t)),
                        upstream: self.upstream.as_ref(),
                        prompt_cache_key: self.prompt_cache_key(),
                        messages,
                        stream: true,
                    };
                    serde_json::to_string(&request)?
                }
                Encoded::Responses(input) => serde_json::to_string(&self.responses_request(
                    input.clone(),
                    tools,
                    &tool_choice,
                    max_tokens,
                    cap.is_some(),
                ))?,
            };
            log::debug!(
                "POST {}: {} turns, {} request bytes, max_tokens {max_tokens}",
                self.url(),
                turns.len(),
                body.len()
            );

            // What this attempt authenticates with, kept so a rejection can say
            // which value was rejected rather than re-reading one that may
            // already have been repaired by another session (`AuthValue`).
            // `current` renews a value near its stated expiry before it is
            // sent, so an OAuth grant is refreshed here rather than on a 401.
            let sent_auth = self.endpoint.auth_value.current().await;
            let conn = self.post();
            if let Some(trace) = trace
                && !sent_reported
            {
                sent_reported = true;
                trace.sent();
            }
            match conn.with_body(body).into_sse().await {
                Ok(events) => break events,
                // The Codex backend streams SSE with no `content-type` at all
                // (docs/transport.md). `into_sse` refuses that like a browser
                // EventSource would, but the failed negotiation still owns the
                // executed conn, so the Responses dialect recovers the stream
                // from it. A *present* wrong content-type, and every other
                // dialect, stay on the failure arm.
                Err(e)
                    if self.dialect() == WireDialect::Responses
                        && matches!(e.kind, SseErrorKind::UnexpectedContentType(None)) =>
                {
                    // `new` fails only on an unexecuted conn (`NoBody`), which a
                    // content-type refusal has already ruled out.
                    match EventStream::new(e.into()) {
                        Ok(events) => break events,
                        Err(e) => {
                            let (status, detail) = Self::sse_open_failure(e).await;
                            return Err(Self::open_error(status, detail));
                        }
                    }
                }
                Err(e) => {
                    let (status, detail) = Self::sse_open_failure(e).await;
                    // The window backstop (`api/reservation.rs`): the estimate
                    // undershot and the provider named the overrun, so halve
                    // the reservation and go again — once per halving, down to
                    // the floor. Any other open failure stays terminal.
                    let overran = cap.is_none()
                        && status == Some(Status::BadRequest)
                        && is_window_overflow(&detail);
                    // The other reservation 400: the cap itself is above what the
                    // model accepts, which is what an *unreported* cap
                    // (`UNKNOWN_OUTPUT_CAP`) risks. Where the provider names its
                    // ceiling the retry goes straight there; where it does not,
                    // the same halving applies.
                    let rejected =
                        (cap.is_none() && status == Some(Status::BadRequest) && !overran)
                            .then(|| output_cap_rejection(&detail))
                            .flatten();
                    // A statement rather than a condition, so the reservation
                    // guard is released here: the `continue` below re-enters the
                    // request path, which reads the reservation again.
                    let retry = match rejected {
                        Some(OutputCapRejection::Max(stated)) => {
                            let mut reservation = self.reservation.lock().unwrap();
                            reservation.clamp_to(stated).or_else(|| reservation.halve())
                        }
                        Some(OutputCapRejection::Unstated) => {
                            self.reservation.lock().unwrap().halve()
                        }
                        None => overran
                            .then(|| self.reservation.lock().unwrap().halve())
                            .flatten(),
                    };
                    if let Some(retry) = retry {
                        if overran {
                            log::warn!(
                                "prompt plus max_tokens {max_tokens} overran the window; \
                                 retrying with max_tokens {retry}"
                            );
                        } else {
                            log::warn!(
                                "{} refused max_tokens {max_tokens}; retrying with \
                                 max_tokens {retry}",
                                self.endpoint.provider
                            );
                        }
                        max_tokens = u32::try_from(retry).unwrap_or(u32::MAX);
                        continue;
                    }
                    // The credential aged out under a running host
                    // (docs/transport.md). The value is shared by every session
                    // on this connection, so re-reading its source here repairs
                    // all of them — the operator runs `codex login` in another
                    // terminal and the next attempt carries the new token. A
                    // credential that is *wrong* rather than expired classifies
                    // Terminal and never reaches this.
                    if !reloaded_auth
                        && status == Some(Status::Unauthorized)
                        && classify_body(&detail) == FailureClass::CredentialExpired
                    {
                        reloaded_auth = true;
                        match self.endpoint.auth_value.reload(&sent_auth).await {
                            Reloaded::Replaced => {
                                log::warn!(
                                    "{} rejected an expired credential; re-read its source \
                                     and retrying",
                                    self.endpoint.provider
                                );
                                continue;
                            }
                            // Every other outcome leaves the same value on the
                            // wire, so the retry would fail identically. The
                            // reason joins the error the caller stalls with.
                            outcome => {
                                return Err(Self::open_error(
                                    status,
                                    format!("{detail} (credential re-read: {outcome})"),
                                ));
                            }
                        }
                    }
                    return Err(Self::open_error(status, detail));
                }
            }
        };

        let mut assembler = match self.dialect() {
            WireDialect::Messages => AnyAssembler::Messages(Assembler::new()),
            WireDialect::Responses => AnyAssembler::Responses(ResponsesAssembler::new()),
            // No configuration names the dialect until its client lands
            // (`WireDialect::from_name`).
            WireDialect::Completions => unreachable!("the Completions dialect is not served yet"),
            WireDialect::SystemOne => {
                unreachable!("a client is only built on a provider that serves turns")
            }
        };
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
        //
        // The operator's interrupt, armed above before the request went out, is the
        // other way out: it ends the read the same way, and `Aborted` tells the
        // caller not to retry.
        let mut operator_abort = false;
        let mut first_byte_reported = false;
        let deadline = tokio::time::Instant::now() + self.stream_ceiling;
        while let Some(event) = tokio::select! {
            next = tokio::time::timeout_at(deadline, events.next()) => match next {
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
            },
            _ = &mut abort => {
                let why = "generation ended by the operator's interrupt".to_string();
                log::info!("{why}");
                assembler.fail(why);
                operator_abort = true;
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
            if let Some(trace) = trace
                && !first_byte_reported
            {
                first_byte_reported = true;
                trace.first_byte();
            }
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
        // Dropping the stream closes its connection: a hung socket is freed here,
        // not when the provider decides.
        drop(events);
        let mut completion = assembler.finish();
        if operator_abort && let Ok(completion) = &mut completion {
            completion.failure_class = FailureClass::Aborted;
        }
        // Anchor the next estimate to this response's exact prompt count
        // (`api/reservation.rs`). An attempt that died with zero usage
        // counted nothing and teaches nothing.
        if let Ok(completion) = &completion {
            let prompt_tokens = completion.usage.prompt_tokens();
            if prompt_tokens > 0 {
                self.reservation
                    .lock()
                    .unwrap()
                    .calibrate(prompt_tokens, body_bytes);
            }
        }
        completion
    }

    /// The turns URL this client posts to. Present by construction: resolution
    /// refuses a provider with no inference dialect for every role that builds a
    /// client (`src/inference/options.rs`), so the only way here is a provider
    /// that has one.
    fn url(&self) -> &str {
        self.endpoint
            .url
            .as_deref()
            .expect("a client is only built on a provider that serves turns")
    }

    /// The authenticated POST every request starts from. The `anthropic-version`
    /// header is part of the Messages dialect, not a provider identity, so it rides
    /// only that dialect (docs/transport.md).
    fn post(&self) -> trillium_client::Conn {
        let conn = self
            .http
            .post(self.url())
            .with_request_header(
                self.endpoint.auth_header.clone(),
                self.endpoint.auth_value.get(),
            )
            .with_request_header("content-type", "application/json");
        let conn = match self.dialect() {
            WireDialect::Messages => {
                conn.with_request_header("anthropic-version", ANTHROPIC_VERSION)
            }
            WireDialect::Responses => conn,
            // No configuration names the dialect until its client lands
            // (`WireDialect::from_name`).
            WireDialect::Completions => unreachable!("the Completions dialect is not served yet"),
            WireDialect::SystemOne => {
                unreachable!("a client is only built on a provider that serves turns")
            }
        };
        let conn = self
            .endpoint
            .extra_headers
            .iter()
            .fold(conn, |conn, (name, value)| {
                conn.with_request_header(name.clone(), value.clone())
            });
        let conn = self.with_attribution(self.with_session_id(conn));
        self.with_turn_routing(conn)
    }

    /// The Codex backend's routing headers (docs/transport.md): the model hint
    /// on every request, and the turn's sticky token once the backend has issued
    /// one. Both are transport-only — neither reaches the body, so neither can
    /// move the cached prefix; what they move is which machine sees it.
    fn with_turn_routing(&self, conn: trillium_client::Conn) -> trillium_client::Conn {
        let provider = &self.endpoint.provider;
        let conn = match provider.routing_hint_header() {
            Some(header) => conn.with_request_header(header, format!("model={}", self.model)),
            None => conn,
        };
        match (provider.turn_routing_header(), self.turn_routing_token()) {
            (Some(header), Some(token)) => conn.with_request_header(header, token),
            _ => conn,
        }
    }

    /// The Responses envelope for one turn (docs/transport.md).
    fn responses_request<'a>(
        &'a self,
        input: Vec<responses::wire::InputItem>,
        tools: &[Value],
        tool_choice: &ToolChoice,
        max_output_tokens: u32,
        capped: bool,
    ) -> responses::ResponsesRequest<'a> {
        let reasoning =
            (self.thinking.adaptive && !self.thinking_off).then(|| responses::Reasoning {
                effort: self.effort.clone(),
                summary: "auto",
            });
        match self.endpoint.provider.responses_profile() {
            ResponsesProfile::Platform => responses::ResponsesRequest {
                model: &self.model,
                instructions: None,
                input,
                tools: responses::tool_definitions(tools),
                tool_choice: responses::tool_choice_value(tool_choice),
                parallel_tool_calls: tool_choice.forced().is_none(),
                store: false,
                include: None,
                truncation: Some("disabled"),
                max_output_tokens: Some(max_output_tokens),
                reasoning,
                temperature: self
                    .temperature
                    .map(|t| self.endpoint.provider.map_temperature(t)),
                prompt_cache_key: self.prompt_cache_key(),
                prompt_cache_options: (self.prompt_cache == PromptCachePolicy::Explicit)
                    .then_some(responses::PromptCacheOptions { mode: "explicit" }),
                stream: true,
            },
            // What Codex CLI sends, and nothing it does not: the backend is
            // undocumented, so an extra field is a guess at its tolerance.
            ResponsesProfile::Codex => responses::ResponsesRequest {
                model: &self.model,
                instructions: Some(&self.system),
                input,
                tools: responses::tool_definitions(tools),
                tool_choice: responses::tool_choice_value(tool_choice),
                parallel_tool_calls: tool_choice.forced().is_none(),
                store: false,
                include: Some(vec!["reasoning.encrypted_content"]),
                truncation: None,
                // Omitted as Codex CLI omits it, except where a caller fixed the cap:
                // the backend enforces one with a minimum of 16 (docs/transport.md).
                max_output_tokens: capped.then_some(max_output_tokens),
                reasoning,
                temperature: None,
                prompt_cache_key: self.prompt_cache_key(),
                prompt_cache_options: None,
                stream: true,
            },
        }
    }

    /// WireTurn a failed `into_sse` into a legible [`Error`], reading the response body on a
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
        // Any open failure still owns the conn: an unexpected content-type (a JSON
        // error body on a 4xx the retry handler let through, or a 200 that is not
        // a stream) is read the same way a status error is, so the upstream's
        // words reach the record instead of "content-type: None".
        if !matches!(e.kind, SseErrorKind::Status(_)) {
            let kind = e.kind.to_string();
            let mut conn: Conn = e.into();
            let status = conn.status();
            let body = match captured_error_body(&mut conn) {
                Some(body) => body,
                // No status means no response arrived — the connect itself
                // failed — so there is no body and no transport to read one
                // from; the client panics on the read rather than erroring.
                None if status.is_none() => String::new(),
                None => conn
                    .response_body()
                    .read_string()
                    .await
                    .unwrap_or_else(|read_err| format!("(could not read body: {read_err})")),
            };
            let body = body.trim();
            let detail = if body.is_empty() {
                kind
            } else {
                format!("{kind} — {body}")
            };
            return (status, detail);
        }
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
        Box::new(OpenError::new(status.map(|s| s as u16), message, &detail))
    }

    /// A cache-keepalive ping (docs/transport.md): a **non-streaming**
    /// round-trip over the same prefix a real turn would send, capped at
    /// [`Provider::keepalive_output_cap`] (`max_tokens: 0` on Anthropic, 16 on
    /// OpenRouter, which reads 0 as unset). Returns only the `usage`; the log is never touched.
    ///
    /// Non-streaming because `max_tokens: 0` is rejected with `stream: true`. The
    /// prefix bytes must match a real request's **exactly** or the ping writes a
    /// *new* cache entry instead of refreshing the live one — which is why it
    /// takes the caller's `messages` / `tools` / `tool_choice` (the loop's
    /// `auto()`) and reuses the client's own `system`, rather than reconstructing
    /// any of them.
    ///
    /// On the Responses dialect the ping is an ordinary streamed request with the
    /// output capped at [`Provider::keepalive_output_cap`] ([`Self::responses_keepalive`]).
    pub async fn keepalive(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<RequestUsage, KeepaliveError> {
        if self.dialect() == WireDialect::Responses {
            return self
                .with_origin(RequestOrigin::Keepalive)
                .responses_keepalive(turns, tools, tool_choice)
                .await;
        }
        let messages = encode_messages(turns, self.image_placement(), self.breakpoint_placement());
        let messages = &messages;
        let thinking = Thinking::for_request(&tool_choice, self.thinking, self.thinking_off);
        let request = Request {
            model: &self.model,
            max_tokens: self.endpoint.provider.keepalive_output_cap(),
            system: &self.system,
            tools,
            tool_choice,
            // The turn's own thinking and effort (docs/transport.md): on Anthropic a
            // ping whose thinking differs from the turn's misses the message-level
            // cache and writes a fresh entry instead of refreshing the live one.
            // `max_tokens: 0` takes thinking on and any effort.
            thinking,
            reasoning: self.reasoning_off(thinking),
            output_config: self.effort.clone().map(|effort| OutputConfig { effort }),
            temperature: self
                .temperature
                .map(|t| self.endpoint.provider.map_temperature(t)),
            upstream: self.upstream.as_ref(),
            prompt_cache_key: self.prompt_cache_key(),
            messages,
            stream: false,
        };
        let body = serde_json::to_string(&request).map_err(KeepaliveError::other)?;
        // Non-streaming, so the ping has no first byte: it is sent, then it ends or fails.
        let trace = Trace::begin(
            self.stages.as_ref(),
            RequestOrigin::Keepalive,
            &self.active_trace,
        );
        if let Some(trace) = &trace {
            trace.sent();
        }
        let result = self.keepalive_exchange(body).await;
        if let Some(trace) = trace {
            match &result {
                Ok(_) => trace.ended(None),
                Err(e) => trace.failed(&e.to_string()),
            }
        }
        result
    }

    /// The Messages keepalive's POST and read, split from [`Self::keepalive`] so the
    /// trace closes on every exit.
    async fn keepalive_exchange(&self, body: String) -> Result<RequestUsage, KeepaliveError> {
        let conn = self.post();
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
        serde_json::from_str::<KeepaliveResponse>(&text)
            .map(KeepaliveResponse::into_request_usage)
            .map_err(KeepaliveError::other)
    }

    /// The Responses keepalive (docs/transport.md): the turn's own envelope, streamed,
    /// with the output capped at [`Provider::keepalive_output_cap`]. The dialect has no
    /// `max_tokens: 0`, and the Codex backend resumes a cached prefix only where an
    /// earlier request ended, so the ping re-sends the last request's input whole —
    /// what the keepalive loop's lopped projection is — and pays a cached read plus
    /// the capped output.
    async fn responses_keepalive(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<RequestUsage, KeepaliveError> {
        let completion = self
            .send(
                turns,
                tools,
                tool_choice,
                Some(self.endpoint.provider.keepalive_output_cap()),
                |_| {},
            )
            .await
            .map_err(|e| match e.downcast_ref::<OpenError>() {
                Some(open) => KeepaliveError::Status {
                    status: open.status(),
                    body: open.to_string(),
                },
                None => KeepaliveError::Other(e),
            })?;
        if let Some(why) = completion.upstream_failure() {
            return Err(KeepaliveError::other(why.to_string()));
        }
        Ok(completion.request_usage())
    }

    /// The triage-lineage primer (backlog/separate-triage-cold-prefix.md): the turn's
    /// own input replayed so a later peek extends a request that ended where the main
    /// one did. Shaped exactly like the keepalive's Responses ping — the turn envelope
    /// with the backend-minimum output cap — because it is the same write: a cached
    /// read plus the capped output. `None` where no implicit lineage exists to plant
    /// in (anything but Responses-with-implicit-policy), so the caller sends nothing
    /// speculative there; the peek then goes out unprimed, the failure mode being fixed.
    ///
    /// The primer wants the write, not the read, so a marked output does not fail it:
    /// a 16-token budget at a call-eliciting position truncates the model's function
    /// call mid-arguments, which marks the completion without unmaking its input
    /// prefix — the provider still processed (and wrote) the whole replayed prefix.
    /// Only a response with no accounted input is evidence nothing was planted.
    pub(crate) async fn prime(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Option<Result<RequestUsage, Error>> {
        if self.dialect() != WireDialect::Responses
            || self.prompt_cache != PromptCachePolicy::Implicit
        {
            return None;
        }
        let completion = match self
            .with_origin(RequestOrigin::TriagePrimer)
            .send(
                turns,
                tools,
                tool_choice,
                Some(self.endpoint.provider.keepalive_output_cap()),
                |_| {},
            )
            .await
        {
            Ok(completion) => completion,
            Err(e) => return Some(Err(e)),
        };
        let usage = completion.request_usage();
        if usage.usage.input_tokens == 0 {
            let why = completion
                .upstream_failure()
                .unwrap_or("primer response carried no usage")
                .to_string();
            return Some(Err(Box::new(std::io::Error::other(why))));
        }
        Some(Ok(usage))
    }
}

/// One request's encoded conversation, per dialect.
enum Encoded {
    Messages(Vec<ApiMessage>),
    Responses(Vec<responses::wire::InputItem>),
}

/// The per-dialect stream folder behind one read loop.
enum AnyAssembler {
    Messages(Assembler),
    Responses(ResponsesAssembler),
}

impl AnyAssembler {
    fn observed_response(&mut self, response: ResponseMeta) {
        match self {
            Self::Messages(a) => a.observed_response(response),
            Self::Responses(a) => a.observed_response(response),
        }
    }

    fn fail(&mut self, why: String) {
        match self {
            Self::Messages(a) => a.fail(why),
            Self::Responses(a) => a.fail(why),
        }
    }

    fn apply(&mut self, event_type: Option<&str>, data: &str) -> Result<Option<Streamed>, Error> {
        match self {
            Self::Messages(a) => a.apply(event_type, data),
            Self::Responses(a) => a.apply(event_type, data),
        }
    }

    fn finish(self) -> Result<Completion, Error> {
        match self {
            Self::Messages(a) => a.finish(),
            Self::Responses(a) => a.finish(),
        }
    }
}

/// The non-streamed Messages body a keepalive ping returns: the id, model and serving
/// upstream, and the `usage` with its inline cost (docs/transport.md).
#[derive(Deserialize)]
pub(crate) struct KeepaliveResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    usage: UsageWithCost,
}

impl KeepaliveResponse {
    pub(crate) fn into_request_usage(self) -> RequestUsage {
        RequestUsage {
            id: self.id,
            model: self.model,
            usage: self.usage.usage,
            receipt: Receipt {
                served_by: self.provider,
                cost: self.usage.cost.into_inline(),
            },
        }
    }
}
