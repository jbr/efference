//! The upstream wire — request/response shapes and the HTTP call, in either dialect
//! (`WireDialect`: Anthropic Messages or OpenAI Responses, docs/transport.md).
//!
//! Streaming. The request `messages` are produced by [`crate::project::project`];
//! the response arrives as a Server-Sent Events stream and is reassembled by
//! [`Assembler`] back into a single [`Payload::Assistant`], so the result folds
//! straight into the log — this is where the log and the transport meet.
//! Reassembly is a pure fold over the event sequence (docs/transport.md: streaming wire);
//! the `Assembler` is unit-tested without a live stream.

use futures_lite::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trillium_client::Client as HttpClient;
use trillium_client::Conn;
use trillium_client::sse::{EventStream, SseError, SseErrorKind};

use crate::log::{AssistantOutput, MessagesOutputBlock, Payload, ToolCall};
use crate::project::ApiMessage;

/// The `anthropic-version` header value. This is part of the Anthropic Messages
/// *wire dialect*, not a provider identity — OpenRouter's `/api/v1/messages` skin
/// speaks the same dialect and tolerates the header — so it rides every request
/// regardless of which `Endpoint` is targeted (docs/transport.md).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default response-token ceiling, standing only where the boot capability
/// lookup reports no cap of its own ([`Client::with_max_output`]). A 1024
/// ceiling cuts a real final answer mid-sentence; 4096 leaves room to conclude
/// while staying bounded.
const MAX_TOKENS: u32 = 4096;

/// Boxed transport error. Non-2xx responses carry the raw body in the message,
/// so a failure is legible without a typed enum.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Which **upstream** serves the model, for an OpenRouter session: an ordered
/// list of endpoint slugs (`google-ai-studio`, `google-vertex/global`, …) and
/// whether OpenRouter may fall back past them.
///
/// Named for the far side of the request, as `upstream_failure` and the
/// stream-abort messages already are, and deliberately *not* "provider" —
/// [`Provider`] is this codebase's anthropic-vs-openrouter choice, a different
/// question one level up. The wire field is `provider` because that is
/// OpenRouter's name for it; the rename is the only place the two meanings
/// touch.
///
/// `allow_fallbacks` is false whenever an order is given — a pin that silently
/// reroutes is not a pin, and the point is being able to say which endpoint
/// served.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub order: Vec<String>,
    pub allow_fallbacks: bool,
}

impl Upstream {
    /// `None` for an empty order, so "no `--upstream` flag" and "an empty list"
    /// are the same request bytes rather than an empty object on the wire.
    pub fn pinned(order: Vec<String>) -> Option<Self> {
        (!order.is_empty()).then_some(Self {
            order,
            allow_fallbacks: false,
        })
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "str::is_empty")]
    system: &'a str,
    /// Tool definitions. Always sent, so the request never toggles the
    /// tools-cache tier (docs/log-and-projection.md). Their *content* must be
    /// byte-stable — `Bash::definition()` builds them from a [`Value`], whose
    /// keys serialize sorted, so it is.
    tools: &'a [Value],
    tool_choice: ToolChoice,
    /// Extended thinking (docs/transport.md). The matrix over `tool_choice`, the
    /// model family, and the capability lookup is [`Thinking::for_request`]; the
    /// three-way adaptive/disabled/omitted split is why this is not a `bool`. Set
    /// once per call-type and never flipped turn-to-turn, so it stays out of the
    /// cached prefix's way.
    #[serde(skip_serializing_if = "Thinking::is_omit")]
    thinking: Thinking,
    /// OpenRouter's normalized reasoning switch, sent as `{"enabled": false}` only
    /// by a client built `without_thinking` on OpenRouter for a foreign model
    /// (docs/redaction-fork.md, docs/transport.md): `thinking` is omitted there
    /// rather than `disabled`, and OpenRouter's default for a reasoning model is
    /// on. A per-client constant, and `None` skips serialization, so no existing
    /// session's bytes move.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningOff>,
    /// How much effort the model spends on the turn (docs/transport.md) — the
    /// level axis beside `thinking`'s type axis. `None` (no `--effort`, no
    /// `[transport] effort`) skips serialization entirely, so a session that
    /// sets no effort sends the bytes it always did. Set once at boot and never
    /// flipped, so a session that does set one still never moves its own prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
    /// Sampling temperature, in the provider's native float range. Omits when
    /// `None` (the session didn't set `--temperature`), letting the API default
    /// (1.0). When set, the value is the provider's mapped float — a per-session
    /// constant, like `model`, so it never toggles in-flight and stays out of the
    /// cached prefix's way.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    /// Which upstream serves the model (docs/transport.md). Serialized as
    /// `provider`, OpenRouter's name for the field; see [`Upstream`] for why the
    /// type is not called that. A per-session constant like `model`, and `None`
    /// skips serialization entirely, so it never moves a cached prefix.
    #[serde(rename = "provider", skip_serializing_if = "Option::is_none")]
    upstream: Option<&'a Upstream>,
    /// The OpenAI-style sticky-cache key, carrying the same session id the
    /// `x-session-id` header does (docs/transport.md). Sent only where the
    /// provider reads it ([`Provider::prompt_cache_key_field`]); a per-session
    /// constant, and `None` skips serialization entirely, so it never moves a
    /// cached prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
    messages: &'a [ApiMessage],
    /// Always `true` — the response is consumed as SSE (docs/transport.md). A
    /// delivery flag rather than a cache-relevant one, though its position in
    /// this struct is pinned along with the rest of the envelope by
    /// `request_envelope_matches_expected_wire_bytes`.
    stream: bool,
}

/// The one shape [`Request::reasoning`] takes: OpenRouter's `reasoning.enabled`
/// turned off.
#[derive(Serialize, Clone, Copy)]
struct ReasoningOff {
    enabled: bool,
}

const THINKING_DISABLED: Thinking = Thinking::Disabled;
const THINKING_ADAPTIVE: Thinking = Thinking::Adaptive {
    display: Some("summarized"),
};

/// Per-process override for adaptive thinking, read once from `EFFERENCE_THINKING`:
/// `adaptive` forces it on, `off`/`disabled` forces it off, `auto` (or unset) defers
/// to the Claude allowlist and the capability lookup. Read once — env is process-stable and thinking must not
/// flip turn-to-turn or it would perturb the cached prefix (docs/transport.md). An
/// unrecognized value warns once and falls back to `auto`.
static THINKING_OVERRIDE: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
    match std::env::var("EFFERENCE_THINKING")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("adaptive") => Some(true),
        Some("off" | "disabled") => Some(false),
        None | Some("" | "auto") => None,
        Some(other) => {
            log::warn!(
                "EFFERENCE_THINKING={other:?} not understood (use adaptive|off|auto); \
                 falling back to the per-model default"
            );
            None
        }
    }
});

/// The Anthropic `usage` object (docs/transport.md). `Default` seeds the
/// [`Assembler`] before `message_start` arrives.
///
/// Every field needs both `#[serde(default)]` and [`null_or_missing`], because
/// providers report a count they have nothing to say about in two different
/// ways: Anthropic omits the field, while OpenRouter-fronted upstreams send it
/// present with an explicit `null`. `default` alone covers only the absent case;
/// a present `null` still hits the `u64` and errors the whole response away.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct Usage {
    #[serde(default, deserialize_with = "null_or_missing")]
    pub input_tokens: u64,
    #[serde(default, deserialize_with = "null_or_missing")]
    pub output_tokens: u64,
    #[serde(default, deserialize_with = "null_or_missing")]
    pub cache_creation_input_tokens: u64,
    #[serde(default, deserialize_with = "null_or_missing")]
    pub cache_read_input_tokens: u64,
}

impl Usage {
    /// The whole prompt as the provider counted it: the uncached remainder plus
    /// what was read from and written to the cache (docs/transport.md).
    /// `input_tokens` alone is only the uncached part on both dialects, so
    /// every consumer that wants "how big was the prompt" — the reservation
    /// anchor, context occupancy — reads this rather than summing the fields.
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    /// Fold another `usage` object from the same response into this one, keeping
    /// the larger count per field (docs/transport.md: the usage merge). Max
    /// rather than "last event wins" because both usage-bearing events report
    /// per-response *totals*, not increments — neither can legitimately report
    /// less than the other for the same field, so a placeholder or zero from
    /// either side is harmless.
    /// Whether every count is zero — the shape of a stream that produced nothing
    /// billable. What separates an aborted attempt with nothing to price from one
    /// that finalized with real usage and then failed to parse (docs/transport.md).
    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_creation_input_tokens == 0
            && self.cache_read_input_tokens == 0
    }

    pub(crate) fn merge_max(&mut self, other: &Usage) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .max(other.cache_creation_input_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .max(other.cache_read_input_tokens);
    }
}

/// Deserialize a `u64` that a provider may send as an explicit JSON `null` (or omit
/// entirely, when paired with `#[serde(default)]`), folding both to 0 — see
/// [`Usage`]. Not part of any wire-byte pin: `Usage` is response telemetry, never
/// projected (docs/log-and-projection.md).
fn null_or_missing<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or_default())
}

/// A parsed assistant turn plus the metadata we surface and log.
#[derive(Debug)]
pub struct Completion {
    /// Always a [`Payload::Assistant`], ready to append to the log.
    pub assistant: Payload,
    /// The upstream response id from `message_start` (Anthropic `msg_…`; an
    /// OpenRouter-fronted request carries its generation id) — the key that joins a
    /// usage record to the provider's own accounting (docs/log-and-projection.md).
    /// Optional defensively: a provider that omits it still completes.
    pub id: Option<String>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    /// How the failure `upstream_failure` reports should be retried
    /// (docs/transport.md): `Terminal` when the provider named a condition no
    /// retry can change, `Transient` for everything else, including a stream
    /// that simply died. Meaningless when there is no failure.
    pub failure_class: FailureClass,
}

/// One request's [`Usage`] paired with its upstream response id — what the daemon
/// persists per request (docs/log-and-projection.md). Carried instead of a bare
/// `Usage` wherever a request's spend is reported upward, so the id survives to the
/// usage record.
#[derive(Debug, Clone)]
pub struct RequestUsage {
    pub id: Option<String>,
    /// The model the response named for itself — recorded per request because the
    /// session's model can change, so no single header attributes every entry
    /// (docs/log-and-projection.md). Optional defensively, like `id`.
    pub model: Option<String>,
    pub usage: Usage,
}

impl Completion {
    /// The turn's output, whichever dialect produced it.
    pub fn assistant_output(&self) -> &AssistantOutput {
        match &self.assistant {
            Payload::Assistant { output, .. } => output,
            other => unreachable!("a completion always carries an assistant payload: {other:?}"),
        }
    }

    /// `Some` when the producing stream died before finalizing (docs/transport.md):
    /// `assistant` is then a record-only partial — append it for audit, never project
    /// it, and treat the request as a retryable transient. Reads the mark off the
    /// payload itself (the assembler sets it there) so appending an unmarked partial
    /// is unrepresentable.
    pub fn upstream_failure(&self) -> Option<&str> {
        match &self.assistant {
            Payload::Assistant {
                upstream_failure: Some(why),
                ..
            } => Some(why.why()),
            _ => None,
        }
    }

    /// The failure `upstream_failure` reports, when the provider named it as one
    /// a retry cannot change (docs/transport.md).
    pub fn terminal_failure(&self) -> Option<&str> {
        (self.failure_class == FailureClass::Terminal)
            .then(|| self.upstream_failure())
            .flatten()
    }

    /// Whether the operator ended this read (docs/control-channel.md): the partial
    /// is a record-only entry like any aborted attempt, but nothing retries it.
    pub fn operator_abort(&self) -> bool {
        self.failure_class == FailureClass::Aborted && self.upstream_failure().is_some()
    }

    /// This completion's [`RequestUsage`] — the per-request record the daemon persists.
    pub fn request_usage(&self) -> RequestUsage {
        RequestUsage {
            id: self.id.clone(),
            model: Some(self.model.clone()),
            usage: self.usage.clone(),
        }
    }
}

/// What a single SSE event contributed to the *live* view, if anything
/// (docs/transport.md). The assembler folds every event into the [`Completion`]
/// regardless; this is the parallel, transport-only channel that lets a client show
/// what the model is doing **while** it does it. One variant per phase a turn can be
/// in, so a turn spent thinking or emitting a large tool-call input is expressible
/// rather than reading as a hung harness.
///
/// **Every variant carries the `index` of the content block it belongs to**, because a
/// streamed fragment is partial delivery *of a specific item* and a client has to know
/// which. Without it a client can only hold one slot per kind, and a response with two
/// tool calls overwrites the first the moment the second opens. `index` is
/// per-response and the loop runs one request at a time, so "the block at `index` of
/// the response now in flight" is unambiguous; the committed `LogEntry` supersedes the
/// whole live set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Streamed {
    /// A fragment of assistant prose.
    Text { index: usize, text: String },
    /// A fragment of extended-thinking text. Never part of the assistant's answer — a client
    /// showing this must mark it as thinking, not prose.
    Thinking { index: usize, text: String },
    /// The model began emitting a tool call. Arrives at `content_block_start`, *before* the
    /// arguments stream, so a client can name the pending call immediately — the arguments alone
    /// can take many seconds to arrive for a large input. `id` is the `tool_use` id the eventual
    /// `ToolResult` and every per-call annotation key on, so a client can bind the live block to
    /// the call it becomes before the call commits.
    ToolStart {
        index: usize,
        id: String,
        name: String,
    },
    /// A fragment of a tool call's JSON input. Deliberately not parsed: it's a partial byte slice
    /// of a JSON document, so the only honest use is progress (that bytes are arriving), not
    /// content.
    ToolInput { index: usize, fragment: String },
}

#[cfg(test)]
mod test;

pub(crate) mod assembler;
mod auth_value;
mod client;
mod decisions;
mod diagnostics;
mod endpoint;
pub(crate) mod failure;
pub(crate) mod plan;
mod provider;
mod render;
mod reservation;
pub(crate) mod responses;
mod thinking;

pub use auth_value::{AuthValue, Reload, Reloaded, Reread, sync_reload};
pub use client::{Client, DEFAULT_STREAM_CEILING, KeepaliveError};
pub use decisions::{Answer, Decisions, DecisionsUsage, Question};
pub use diagnostics::{
    RetrySurface, TurnRoutingState, UpstreamAttemptFailure, UpstreamAttemptQueue, UpstreamFailure,
    UpstreamFailureState, UpstreamRetry, UpstreamRetryCell, UpstreamRetryState,
    drain_upstream_attempts, push_upstream_attempt, turn_routing_state, upstream_attempt_queue,
    upstream_failure_state, upstream_retry_state,
};
pub(crate) use endpoint::codex_cached_credentials;
pub use endpoint::{Endpoint, ProbeResponse, sticky_session_id};
pub use failure::{FailureClass, OpenError, error_class};
pub use plan::{PlanLimit, PlanSnapshot, PlanState, PlanWindow, plan_state};
#[cfg(test)]
pub(crate) use provider::seed_codex_catalog;
pub use provider::{
    ModelCapabilities, OUTPUT_FLOOR, PromptCachePolicy, Provider, ResponsesProfile, WireDialect,
};
pub use render::{
    render_messages_pretty, render_messages_yaml, render_request_pretty, render_request_yaml,
    render_responses_request_pretty,
};
pub use reservation::{
    OutputCapRejection, Reservation, ReservationState, UNKNOWN_OUTPUT_CAP, is_window_overflow,
    output_cap_rejection, reservation_state,
};
pub use responses::encode_responses;
pub use thinking::{ToolChoice, auto_turns_disable_thinking, check_effort_thinking};

use assembler::Assembler;
use thinking::{OutputConfig, Thinking};
