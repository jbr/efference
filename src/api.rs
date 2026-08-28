//! The Anthropic Messages API wire — request/response shapes and the HTTP call.
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
use trillium_client::sse::{SseError, SseErrorKind};
use trillium_rustls::RustlsConfig;
use trillium_tokio::ClientConfig;

use crate::log::{CallId, ContentBlock, Payload, ToolCall};
use crate::project::ApiMessage;

/// The `anthropic-version` header value. This is part of the Anthropic Messages
/// *wire dialect*, not a provider identity — OpenRouter's `/api/v1/messages` skin
/// speaks the same dialect and tolerates the header — so it rides every request
/// regardless of which `Endpoint` is targeted (docs/transport.md).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Response-token ceiling per request. 1024 truncated real final answers in a
/// live run (the model's synthesis was cut mid-sentence); 4096 gives the agent
/// room to conclude while staying bounded. A `Client` field can replace this when
/// per-call control is needed.
const MAX_TOKENS: u32 = 4096;

/// Boxed error for the Phase 0 transport. A typed error enum can replace this
/// once error handling needs structure; for now non-2xx responses carry
/// the raw body in the message so failures are legible.
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
    /// `None` for an empty order, so "no `--provider` flag" and "an empty list"
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
    /// Tool definitions. Always sent (Phase 0 always has bash), so the request
    /// never toggles the tools-cache tier (docs/log-and-projection.md). Their *content* must be
    /// byte-stable — `Bash::definition()` builds them from a [`Value`], whose
    /// keys serialize sorted, so it is.
    tools: &'a [Value],
    tool_choice: ToolChoice,
    /// Extended thinking (docs/transport.md, docs/transport.md). Rides on
    /// `tool_choice`, the model, and the capability lookup — the matrix is
    /// [`Thinking::for_request`]: adaptive on the main agent turn when the model takes
    /// it (recognized Claude tiers, or a foreign model whose metadata lists
    /// `reasoning`), explicit `disabled` only ever on Claude (forced `tool_choice` is
    /// incompatible with thinking there — 400 — and the **spotter** forces its `spot`
    /// tool; the fork does not force, because flipping `tool_choice` between forced and
    /// `auto` moved the request prefix and invalidated the cache every peek —
    /// docs/redaction-fork.md), and
    /// the field **omitted** for every other foreign case — an explicit `disabled` is
    /// rejected outright by mandatory-reasoning endpoints. Keepalive is handled
    /// independently (`max_tokens: 0` rejects `enabled`; adaptive there is untested).
    /// Set once per call-type and never flipped turn-to-turn, so it stays out of the
    /// cached prefix's way (toggling thinking keeps the tools+system tier valid;
    /// docs/log-and-projection.md). (`disabled` also 400s on Fable 5, which can't
    /// disable thinking — not a serve target yet.)
    #[serde(skip_serializing_if = "Thinking::is_omit")]
    thinking: Thinking,
    /// Sampling temperature, in the provider's native float range. Omits when
    /// `None` (the session didn't set `--temperature`), letting the API default
    /// (1.0). When set, the value is the provider's mapped float — a per-session
    /// constant, like `model`, so it never toggles in-flight and stays out of the
    /// cached prefix's way.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    /// Which upstream serves the model (docs/transport.md) — omitted entirely
    /// for Anthropic, which has no such choice, and for OpenRouter sessions that
    /// did not ask for one. When set, it pins the endpoint instead of letting
    /// OpenRouter choose by price. Serialized as `provider`, OpenRouter's name
    /// for the field; see [`Upstream`] for why the type is not called that.
    ///
    /// Exists because endpoint choice is a *measurable* variable, not a
    /// preference: the endpoints behind one OpenRouter model slug differ in
    /// uptime and latency tail, and on 2026-08-28 an eval run against
    /// `google/gemini-3.7-flash` took eight consecutive mid-stream aborts on a
    /// Vertex endpoint whose p99 latency sat near the client timeout, while an
    /// equally-priced Google AI Studio endpoint was at 99.95%. A run that cannot
    /// say which endpoint served it cannot be compared to another
    /// (eval/harness/README.md).
    ///
    /// A per-session constant like `model`, so it never toggles in flight and
    /// stays out of the cached prefix's way.
    #[serde(rename = "provider", skip_serializing_if = "Option::is_none")]
    upstream: Option<&'a Upstream>,
    /// The OpenAI-style sticky-cache key, carrying this session's sticky-routing
    /// id — the same value that rides the `x-session-id` header
    /// (docs/transport.md). Sent only where the provider reads it
    /// ([`Provider::prompt_cache_key_field`]); `None` skips serialization
    /// entirely, so an Anthropic session's bytes are unmoved by this field's
    /// existence.
    ///
    /// Redundant with the header **by design**, and third in OpenRouter's own
    /// precedence order (body `session_id`, then `x-session-id`, then this —
    /// verified against their prompt-caching docs, 2026-08-28). It is in the
    /// body because the header is the part of the request an intermediary may
    /// drop, and because upstreams that read the OpenAI field directly pin their
    /// own cache on it rather than on OpenRouter's routing key.
    ///
    /// A per-session constant like `model` and `upstream`, so it never toggles
    /// in flight and stays out of the cached prefix's way.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
    messages: &'a [ApiMessage],
    /// Always `true` — we consume the response as SSE (docs/transport.md). Set once and
    /// never varied; a delivery flag, not part of the cached prefix (docs/log-and-projection.md),
    /// so it never invalidates the cache and isn't covered by the wire-bytes pin
    /// (which guards `project()` output, not this request envelope).
    stream: bool,
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

/// The Anthropic `usage` object. The two `cache_*` fields are the Phase 0 gate
/// metrics; defaulted because they're absent unless caching is in play. `Default`
/// seeds the [`Assembler`] before `message_start` arrives.
///
/// Every field is null-tolerant (docs/transport.md: providers). Anthropic
/// *omits* a token count it has nothing to say about; OpenRouter's skin (and the
/// upstreams behind it — e.g. Together) instead sends the field **present with an
/// explicit `null`** (`cache_creation_input_tokens: null` in every `message_start`).
/// `#[serde(default)]` covers only an *absent* field — a present `null` still hits
/// the `u64` and fails (`invalid type: null, expected u64`), which crashed the very
/// first OpenRouter turn. [`null_or_missing`] folds both absent and null to 0, so a
/// token count no provider reports is simply zero telemetry rather than a hard error.
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
    /// Fold another `usage` object from the same response into this one, keeping
    /// the larger count per field. Used by the [`Assembler`] to merge the
    /// `message_start` and `message_delta` usages without either event's
    /// placeholder clobbering the other's real number: Anthropic's
    /// `message_start` carries the input/cache counts and `output_tokens: 1`,
    /// while OpenRouter's carries all-zero/null and puts every real count in
    /// `message_delta`. Max rather than "last non-zero wins" because these are
    /// per-response totals, not increments — neither event can legitimately
    /// report *less* than the other for the same field.
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
/// entirely, when paired with `#[serde(default)]`), folding both to 0. Anthropic
/// omits; OpenRouter-fronted upstreams null it out — see [`Usage`]. Not part of any
/// wire-byte pin: `Usage` is response/telemetry only, never projected (docs/log-and-projection.md).
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
            } => Some(why),
            _ => None,
        }
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
/// (docs/transport.md — the invisible-generation-phase defect). The assembler folds
/// every event into the [`Completion`] regardless; this is the parallel, transport-only channel
/// that lets a client show what the model is doing **while** it does it.
///
/// Before this existed the sink carried a bare prose fragment, so only `text_delta` could reach it
/// — a turn spent thinking or emitting a large tool-call input produced no output at all and read
/// as a hung harness. Typing the channel is what makes the other phases expressible; each variant
/// is one phase a turn can be in.
/// **Every variant carries the `index` of the content block it belongs to**, because a
/// streamed fragment is partial delivery *of a specific item* and a client has to know which.
/// Without it a client can only hold one slot per kind, and a response with two tool calls
/// overwrites the first the moment the second opens — the arriving input a client had been
/// showing simply disappears. That is the same single-slot failure thinking already hit once
/// (docs/tui.md); the slot is the defect, not the field it happened to be in.
///
/// The provider scopes its own stream this way — the assembler already keys blocks by `index`
/// and reads a `tool_use` block's `id` — so this preserves that scoping rather than inventing
/// one. `index` is per-response, and the loop runs one request at a time, so "the block at
/// `index` of the response now in flight" is unambiguous; the committed `LogEntry` supersedes
/// the whole live set.
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

mod assembler;
mod client;
mod diagnostics;
mod endpoint;
mod provider;
mod render;
mod thinking;

pub use client::{Client, DEFAULT_STREAM_CEILING};
pub use diagnostics::{
    RetrySurface, UpstreamAttemptFailure, UpstreamAttemptQueue, UpstreamFailure,
    UpstreamFailureState, UpstreamRetry, UpstreamRetryCell, UpstreamRetryState,
    drain_upstream_attempts, push_upstream_attempt, upstream_attempt_queue, upstream_failure_state,
    upstream_retry_state,
};
pub use endpoint::{Endpoint, sticky_session_id};
pub use provider::{ModelCapabilities, Provider};
pub use render::{
    render_messages_pretty, render_messages_yaml, render_request_pretty, render_request_yaml,
};
pub use thinking::ToolChoice;

use assembler::Assembler;
use thinking::Thinking;
