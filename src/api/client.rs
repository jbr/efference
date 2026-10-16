//! The streaming client: `complete` and the cache `keepalive` (docs/transport.md).
//!
//! This file holds the client's configuration. `complete` and its forced-choice
//! fallback are `client/forced_choice.rs`, one request's pipeline is `client/send.rs`,
//! the keepalive and primer are `client/keepalive.rs`, the per-response observer is
//! `client/upstream_status.rs`, and the image-refusal recovery is `client/images.rs`.

use super::*;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use trillium::Status;
use trillium_client::ClientHandler;
use trillium_client_retry::RetryHandler;

use crate::api::assembler::ResponseMeta;
use crate::api::auth_value::Reloaded;
use crate::api::completions::assembler::CompletionsAssembler;
use crate::api::failure::{FailureClass, OpenError, classify_body};
use crate::api::plan::{PlanState, plan_from_headers, plan_state};
use crate::api::responses::assembler::ResponsesAssembler;
use crate::api::stages::{ActiveTrace, StageSink, Trace};
use crate::api::thinking::ThinkingSupport;
use crate::log::{RequestOrigin, is_captured_header};
use crate::project::{WireTurn, encode_messages};

mod forced_choice;
mod images;
mod keepalive;
mod send;
mod upstream_status;

pub use keepalive::KeepaliveError;
#[cfg(test)]
pub(crate) use keepalive::KeepaliveResponse;
use upstream_status::{CapturedErrorBody, UpstreamStatusHandler, read_error_body, response_meta};
#[cfg(test)]
pub(super) use upstream_status::{clear_upstream_failure, record_upstream_failure};

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

/// The non-2xx statuses the transport retries on its own, before the agent loop
/// sees a failure (docs/transport.md): the rate-limit and server-side family.
const RETRY_STATUSES: [Status; 5] = [
    Status::TooManyRequests,
    Status::InternalServerError,
    Status::BadGateway,
    Status::ServiceUnavailable,
    Status::GatewayTimeout,
];

/// The previous response's id on one client's cache chain (docs/transport.md): the
/// value the next request names as `diagnostics.previous_message_id`. A named type
/// because a bare `Mutex<Option<String>>` in a struct field says nothing about what
/// the string is, and no compile error would follow from passing it the wrong one.
type CacheChain = Arc<Mutex<Option<String>>>;

/// An empty chain — no response to compare against yet, so the first request of a
/// session opts in with an explicit `null` (docs/transport.md).
fn cache_chain() -> CacheChain {
    Arc::new(Mutex::new(None))
}

/// A thin Anthropic Messages client, holding the resolved endpoint, the model,
/// and the session's system prompt. The system is byte-stable across the
/// session, so it stays in the cached prefix (docs/log-and-projection.md); the
/// fork (docs/triage.md) shares the same endpoint and system, so its
/// prefix matches and hits cache.
///
/// `Clone` is a cheap hand-off of *the same* client: the Arc-backed live states
/// (reservation, upstream retry and failure, the cache chain) are shared, so a
/// clone observes and joins the same retry story the original does — and the keepalive
/// ping, which is a clone, names the same previous response the turn did.
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
    /// Never think, whatever the model would do on its own (docs/triage.md):
    /// the separate-model triage's default, where the triage's reasoning is
    /// discarded with the tail and costs latency and tokens on every triage.
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
    /// turn loop and the triage call the same [`Self::complete`].
    stream_ceiling: Duration,
    /// The operator's interrupt, when this client's reads answer to one
    /// (docs/control-channel.md): `POST /loop/interrupt` fires it, and a read in
    /// flight ends there — the stream is dropped, which is what frees a hung
    /// socket, and the partial comes back marked [`FailureClass::Aborted`].
    /// `None` for clients nothing interrupts (the spotter, the dev verbs).
    abort: Option<Arc<tokio::sync::Notify>>,
    /// The session's effort level, when one is set (docs/transport.md). Rides
    /// every request this client makes — the turn loop and the triage, so the
    /// triage's prefix still matches the agent lineage's (docs/triage.md),
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
    /// clones, goes as `auto` with [`forced_choice::forced_choice_instruction`]
    /// instead. Shared across clones, so the lesson is learned once per client.
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
    /// rides on the client because the triage retries with a `Client` and no
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
    /// This client's cache chain, when it has one: the id of the previous response, which
    /// the next request names as `diagnostics.previous_message_id` (docs/transport.md).
    /// Shared across clones, so a keepalive ping, a `with_origin` clone, reads and
    /// extends the same chain the turn does. `None` where this client's requests are off
    /// the chain entirely ([`Self::without_cache_chain`]); the inner id is `None` until
    /// the first response.
    cache_chain: Option<CacheChain>,
    /// The request in flight, for the retry handler (`api/stages.rs`).
    active_trace: ActiveTrace,
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
            .with_statuses(RETRY_STATUSES)
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
            cache_chain: Some(cache_chain()),
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
    /// trajectory start and each queued user turn delivered mid-task; the triage shares this
    /// client and so the token, which is the point — the triage is a request of the
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
    /// as a state topic; the turn loop and the triage write it.
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

    /// The request body's `diagnostics` object, when this client's requests are the
    /// ones a cache chain can be drawn through (docs/transport.md): the provider reads
    /// the field ([`Provider::cache_diagnostics_field`]) and this request sends the
    /// loop's own prompt ([`RequestOrigin::sends_the_loop_prompt`]).
    ///
    /// The object is present — with an explicit `null` on the first request — wherever
    /// those hold, because the object *is* the opt-in that stores the fingerprint the
    /// next request is compared against; a chain that skips a request loses the
    /// comparison for the one after it. `None` where they do not: a provider that would
    /// 400 or answer a meaningless null, and a request whose body is not the loop's
    /// prompt, which could only ever compare unlike bodies.
    fn cache_diagnostics(&self) -> Option<CacheDiagnostics> {
        let chain = self.cache_chain.as_ref()?;
        (self.origin.sends_the_loop_prompt() && self.endpoint.provider.cache_diagnostics_field())
            .then(|| CacheDiagnostics {
                previous_message_id: chain.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            })
    }

    /// Record the response that just arrived as the one this chain's next request is
    /// compared against (docs/transport.md). `origin` is the request that produced it,
    /// and only a **turn** extends the chain: a turn's response is the one whose
    /// fingerprint is the loop's own prompt, so it is the only body the next turn can be
    /// held against. A keepalive ping still *sends* the object — that is how its own
    /// response gets a comparison, and the ping is the crispest probe of the turn's
    /// prefix — but its body differs in `max_tokens` and in being non-streaming, so
    /// naming it as the next turn's comparison base would risk the provider answering
    /// `unavailable` for a difference that is ours and not the prefix's.
    ///
    /// A triage never reaches here: its origin does not send the loop's prompt, so it
    /// neither opts in nor is remembered. Neither does a client taken off the chain
    /// with [`Self::without_cache_chain`], whatever origin it carries.
    pub(super) fn remember_response_id(&self, origin: RequestOrigin, id: Option<&str>) {
        let (Some(chain), RequestOrigin::Turn, Some(id)) = (self.cache_chain.as_ref(), origin, id)
        else {
            return;
        };
        *chain.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
    }

    /// This client, off the cache chain (docs/transport.md): it sends no `diagnostics`
    /// object and records nothing. For a request that carries a turn's origin but not the
    /// turn's prompt — the forced-choice fallback appends its own instruction turn with
    /// thinking off, so its response is not the body the next turn extends, and naming it
    /// as the comparison base would have the provider report a divergence in `messages`
    /// that the loop never made.
    pub fn without_cache_chain(mut self) -> Self {
        self.cache_chain = None;
        self
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
            .endpoint
            .authorize(self.http.post(self.url()), self.endpoint.auth_value.get())
            .with_request_header("content-type", "application/json");
        let conn = match self.dialect() {
            WireDialect::Messages => {
                conn.with_request_header("anthropic-version", ANTHROPIC_VERSION)
            }
            WireDialect::Responses => conn,
            WireDialect::Completions => conn,
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

    /// The session's `--temperature` in the provider's native float range, as a
    /// request body carries it; `None` when the session set none.
    fn wire_temperature(&self) -> Option<f64> {
        self.temperature
            .map(|t| self.endpoint.provider.map_temperature(t))
    }

    /// Pin which upstream serves this session (docs/transport.md). Applies to
    /// every request the client makes, so a session's spotter and triages
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
    /// the triage carry the same envelope; the keepalive is not built from it.
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
    /// own (docs/triage.md): the separate-model triage's default. Spelled
    /// the way the model's metadata says ([`ThinkingSupport::off`]), so set the
    /// capability answer with [`Self::with_thinking`] as well.
    pub fn without_thinking(mut self) -> Self {
        self.thinking_off = true;
        self
    }

    /// Set how long one response's SSE stream may stay open before the read loop
    /// gives up on it (`[transport] stream-ceiling`, docs/transport.md). Applies
    /// to every request this client makes, so a session's turns and its fork
    /// triages are bounded alike.
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

    /// A client whose reads do not answer to the operator's interrupt: for a
    /// detached request that is not part of the trajectory the interrupt stops
    /// (docs/consolidate.md).
    pub fn without_interrupt(mut self) -> Self {
        self.abort = None;
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
}
