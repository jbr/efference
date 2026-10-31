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

use crate::log::{
    AssistantOutput, Exclusion, MessagesOutputBlock, Payload, Provider, ToolCall, WireDialect,
};
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

/// The Anthropic cache-diagnostics opt-in (docs/transport.md): the top-level
/// `diagnostics` object, whose `previous_message_id` names the response this
/// request is to be compared against. The object *is* the opt-in — the API
/// stores a fingerprint for a request only when it carries one — so a client
/// that wants the comparison sends it on every request of a chain.
///
/// `previous_message_id` is `None` on the first request of a chain, and that
/// `null` is the opt-in without anything to compare against. It never skips
/// serialization: absent and explicit-null are different bytes, and the null is
/// the whole point of the first request.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CacheDiagnostics {
    pub previous_message_id: Option<String>,
}

/// The response half of the same object (docs/transport.md): the `diagnostics` field a
/// response carries when its request opted in. One struct for both read sites — the
/// streamed `message_start` frame and the non-streamed keepalive body — so the provider's
/// shape has one spelling here.
///
/// The nesting is what the provider distinguishes: the whole object absent or `null`
/// means no comparison was made, while a present object whose `cache_miss_reason` is
/// `null` means the comparison had not finished when the response serialized. Both read
/// as no attribution — the second resolves on the next request, whose comparison is
/// against the fingerprint this response stored.
#[derive(Clone, Debug, Deserialize)]
pub struct CacheDiagnosticsResult {
    #[serde(default)]
    pub cache_miss_reason: Option<CacheMiss>,
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
    /// Extended thinking (docs/transport.md). Chosen by [`Thinking::for_request`]
    /// from `tool_choice` and the model's metadata ([`ThinkingSupport`]); the
    /// model-dependent spellings of off are why this is not a `bool`. Set once per
    /// call-type and never flipped turn-to-turn, so it stays out of the cached
    /// prefix's way.
    #[serde(skip_serializing_if = "Thinking::is_omit")]
    thinking: Thinking,
    /// OpenRouter's normalized reasoning switch, sent as `{"enabled": false}` only
    /// by a client built `without_thinking` on OpenRouter where `thinking` is
    /// omitted (docs/triage.md, docs/transport.md): a model whose
    /// reasoning is mandatory or undeclared, whose default is on. A per-client constant, and `None` skips serialization, so no existing
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
    /// The cache-diagnostics opt-in for this request (docs/transport.md), sent only
    /// where the provider reads it ([`Provider::cache_diagnostics_field`]). `None`
    /// skips serialization entirely, so a session that gets no object sends the
    /// bytes it always did.
    ///
    /// This is the envelope's only *per-request* field: its value changes every
    /// turn, which is why it sits after `messages`, outside the cached prefix.
    /// Measured rather than assumed — a pair of requests whose bodies differ only
    /// in this value read the whole 15k-token prefix on the second one, so the
    /// field is not part of what the cache matches (docs/transport.md).
    #[serde(rename = "diagnostics", skip_serializing_if = "Option::is_none")]
    cache_diagnostics: Option<CacheDiagnostics>,
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

const THINKING_ADAPTIVE: Thinking = Thinking::Adaptive {
    display: Some("summarized"),
};

/// The Anthropic `usage` object (docs/transport.md). `Default` seeds the
/// [`Assembler`] before `message_start` arrives.
///
/// Every field needs both `#[serde(default)]` and [`null_or_missing`], because
/// providers report a count they have nothing to say about in two different
/// ways: Anthropic omits the field, while OpenRouter-fronted upstreams send it
/// present with an explicit `null`. `default` alone covers only the absent case;
/// a present `null` still hits the `u64` and errors the whole response away.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

/// A cache miss the provider itself attributed to a divergence (docs/transport.md) —
/// one `diagnostics.cache_miss_reason` object off a response. It names *which part* of
/// the request the provider saw differ from the request it compared against, which no
/// token total can: the four `*_changed` reasons are the projection, the system prompt,
/// the tools array, and the message history, in the provider's own comparison order.
///
/// `cache_missed_input_tokens` is the provider's estimate of how much prefix fell after
/// the divergence point. It is derived from byte lengths before tokenization, so it is
/// a magnitude rather than a billing number.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CacheMiss {
    #[serde(rename = "type")]
    pub reason: CacheMissReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_missed_input_tokens: Option<u64>,
}

/// Which divergence the provider named (docs/transport.md). The first four are
/// divergences and identify the *first* one only — the provider reports the earliest,
/// so a later one may hide behind it. The last two mean no comparison was produced at
/// all, and are not evidence that the prefix moved.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CacheMissReason {
    /// A different model served the request; the cache is per model.
    ModelChanged,
    /// The system prompt differs — typically a per-request value interpolated into it.
    SystemChanged,
    /// The tools array differs: added, removed, reordered, or serialized
    /// non-deterministically. The divergence that has cost this harness a cache before
    /// (docs/log-and-projection.md).
    ToolsChanged,
    /// Model, system, and tools all match, but an earlier entry in `messages` was
    /// altered, reordered, or removed rather than appended to.
    MessagesChanged,
    /// No fingerprint was stored for the id we named — the previous request did not
    /// carry the object, or it aged out of the provider's retention.
    PreviousMessageNotFound,
    /// No comparison was produced: a prompt-affecting parameter outside model/system/
    /// tools/messages differs (docs/transport.md), or the divergence sits beyond the
    /// provider's comparison horizon.
    Unavailable,
    /// A reason this build does not know. Kept so a new provider value reads as an
    /// unnamed miss rather than failing the whole response.
    #[serde(other)]
    Other,
}

impl CacheMissReason {
    /// Whether this reason *is* a divergence — the four the provider names by location,
    /// as opposed to the two that report no comparison happened.
    ///
    /// The split matters where a reason is counted as attribution: `Unavailable` and
    /// `PreviousMessageNotFound` say nothing about the prefix, so a cold read carrying
    /// one is still unexplained ([`crate::daemon::CacheRead`]).
    pub fn names_a_divergence(self) -> bool {
        matches!(
            self,
            Self::ModelChanged | Self::SystemChanged | Self::ToolsChanged | Self::MessagesChanged
        )
    }
}

impl std::fmt::Display for CacheMissReason {
    /// The provider's own spelling, so a report and a raw response line agree.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::ModelChanged => "model_changed",
            Self::SystemChanged => "system_changed",
            Self::ToolsChanged => "tools_changed",
            Self::MessagesChanged => "messages_changed",
            Self::PreviousMessageNotFound => "previous_message_not_found",
            Self::Unavailable => "unavailable",
            Self::Other => "other",
        };
        f.write_str(name)
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
    /// Why the provider ended the output, when it said ([`Finish`]).
    pub finish: Option<Finish>,
    pub usage: Usage,
    /// The provider's own attribution of a cache miss for this request ([`CacheMiss`]),
    /// when it named one (docs/transport.md) — `diagnostics.cache_miss_reason` off the
    /// response. `None` covers all three of "no object was sent", "a comparison ran and
    /// found no divergence", and "the comparison was still running when the response
    /// serialized": the provider's `null` and its `{"cache_miss_reason": null}` are the
    /// same fact to every consumer here, because the last of them resolves on the next
    /// request — whose comparison is against the fingerprint *this* response stored.
    pub cache_miss: Option<CacheMiss>,
    /// What the response said about who served it and what it billed
    /// ([`Receipt`]); empty on providers that do not say.
    pub receipt: Receipt,
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
    /// The provider's own attribution of a cache miss for this request, when it named
    /// one ([`Completion::cache_miss`]) — persisted with the counts because a cold read
    /// and its stated cause are one fact about one request.
    pub cache_miss: Option<CacheMiss>,
    pub receipt: Receipt,
}

impl RequestUsage {
    /// A request's token counts with no id, model, attribution, or receipt — the fields
    /// a test or a route with nothing to report leaves unset. Adding a field to the
    /// struct changes this constructor, not its callers.
    pub fn new(usage: Usage) -> Self {
        Self {
            id: None,
            model: None,
            usage,
            cache_miss: None,
            receipt: Receipt::default(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_receipt(mut self, receipt: Receipt) -> Self {
        self.receipt = receipt;
        self
    }

    /// Attach the provider's attribution of a cache miss, as a response that named one
    /// would ([`Completion::cache_miss`]).
    pub fn with_cache_miss(mut self, cache_miss: CacheMiss) -> Self {
        self.cache_miss = Some(cache_miss);
        self
    }
}

/// What a response reports about its own provenance and price, beside its token
/// counts (docs/transport.md). Kept off [`Usage`] so that type stays the token
/// counts both dialects share; the fields here come from an aggregator
/// (OpenRouter's Messages endpoint) and are absent on Anthropic direct and most
/// other providers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Receipt {
    /// The upstream that served the request, as the message's `provider` field
    /// names it. Not [`Provider`] (efference's own provider enum) and not the
    /// `--upstream` routing pin, which is the request-side preference this reports
    /// the outcome of.
    pub served_by: Option<String>,
    /// The cost the response billed inline.
    pub cost: Option<InlineCost>,
}

/// The billed cost an aggregator returns inside `usage` (`usage.cost` and
/// `usage.cost_details`), in USD. Distinct from the generation-lookup price
/// (docs/transport.md), which is the fallback when a response carries none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineCost {
    pub total_usd: f64,
    /// The prompt share (`cost_details.upstream_inference_prompt_cost`), when reported.
    pub prompt_usd: Option<f64>,
    /// The completion share (`cost_details.upstream_inference_completions_cost`).
    pub completion_usd: Option<f64>,
}

/// The cost-bearing fields of a `usage` object, read alongside [`Usage`] from the
/// same JSON. Null and absent both read as no cost.
#[derive(Deserialize, Default)]
pub(crate) struct UsageCost {
    #[serde(default)]
    cost: Option<f64>,
    #[serde(default)]
    cost_details: Option<CostDetails>,
}

#[derive(Deserialize, Default)]
struct CostDetails {
    #[serde(default)]
    upstream_inference_prompt_cost: Option<f64>,
    #[serde(default)]
    upstream_inference_completions_cost: Option<f64>,
}

impl UsageCost {
    pub(crate) fn into_inline(self) -> Option<InlineCost> {
        let details = self.cost_details.unwrap_or_default();
        Some(InlineCost {
            total_usd: self.cost?,
            prompt_usd: details.upstream_inference_prompt_cost,
            completion_usd: details.upstream_inference_completions_cost,
        })
    }
}

/// A `usage` object read as its token counts and its inline cost together.
#[derive(Deserialize)]
pub(crate) struct UsageWithCost {
    #[serde(flatten)]
    pub(crate) usage: Usage,
    #[serde(flatten)]
    pub(crate) cost: UsageCost,
}

impl Completion {
    /// The output cap cut the response.
    pub fn hit_output_cap(&self) -> bool {
        self.finish
            .as_ref()
            .is_some_and(|finish| finish.reason == FinishReason::OutputCap)
    }

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
                excluded: Some(Exclusion::Upstream(why)),
                ..
            } => Some(why.why()),
            _ => None,
        }
    }

    /// A finalized turn with neither prose nor a call — reasoning alone, or text that is
    /// empty or whitespace — that the output cap did not cut (docs/transport.md). It ends
    /// nothing a reader can act on, so the loop keeps it record-only and asks again. A
    /// turn the cap cut is not one: asked again, it would meet the same cap.
    pub fn says_nothing(&self) -> bool {
        let output = self.assistant_output();
        !self.hit_output_cap()
            && output.text().iter().all(|text| text.trim().is_empty())
            && output.tool_calls().is_ok_and(|calls| calls.is_empty())
    }

    /// Mark a completion that [says nothing](Self::says_nothing) record-only, as the
    /// empty completion it is.
    pub(crate) fn exclude_as_empty(&mut self) {
        if let Payload::Assistant { excluded, .. } = &mut self.assistant {
            *excluded = Some(Exclusion::Empty);
        }
    }

    /// Mark a completion that streamed cleanly as one no retry can repair: its turn
    /// becomes record-only, and the loop stalls with `why`.
    pub(crate) fn fail_terminal(&mut self, why: String) {
        if let Payload::Assistant { excluded, .. } = &mut self.assistant {
            *excluded = Some(Exclusion::Upstream(why.into()));
        }
        self.failure_class = FailureClass::Terminal;
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
            cache_miss: self.cache_miss.clone(),
            receipt: self.receipt.clone(),
        }
    }
}

/// What a single SSE event contributed to the *live* view, if anything
/// (docs/transport.md). The assembler folds every event into the [`Completion`]
/// regardless; this is the parallel, transport-only channel that lets a client show
/// what the model is doing **while** it does it. One variant per output kind a turn can
/// stream, so a turn spent thinking or emitting a large tool-call input is expressible
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
pub(crate) mod completions;
mod decisions;
mod diagnostics;
mod endpoint;
pub(crate) mod failure;
mod finish;
pub(crate) mod plan;
mod provider;
mod render;
mod request_options;
mod reservation;
pub(crate) mod responses;
mod stages;
#[cfg(test)]
mod testing;
mod thinking;
mod truncation;

pub use auth_value::{AuthValue, Reload, Reloaded, Reread, sync_reload};
pub use client::{Client, DEFAULT_STREAM_CEILING, KeepaliveError, SentRequest};
pub use completions::{
    encode_completions, render_completions_request_pretty, translate_completions,
};
pub use decisions::{Answer, Decisions, DecisionsUsage, Instructions, Question};
pub use diagnostics::{
    RetrySurface, TurnRoutingState, UpstreamAttemptFailure, UpstreamAttemptQueue, UpstreamFailure,
    UpstreamFailureState, UpstreamRetry, UpstreamRetryCell, UpstreamRetryState,
    drain_upstream_attempts, push_upstream_attempt, turn_routing_state, upstream_attempt_queue,
    upstream_failure_state, upstream_retry_state,
};
pub(crate) use endpoint::codex_cached_credentials;
pub use endpoint::{Endpoint, ProbeResponse, sticky_session_id};
pub use failure::{FailureClass, OpenError, error_class, error_status, is_request_rejection};
pub use finish::{Finish, FinishReason};
pub use plan::{PlanLimit, PlanSnapshot, PlanState, PlanWindow, plan_state};
#[cfg(test)]
pub(crate) use provider::seed_codex_catalog;
pub use provider::{
    ListedModel, ModelCapabilities, ModelKind, OUTPUT_FLOOR, Price, PromptCachePolicy,
    ResponsesProfile, TokenPrices,
};
pub use render::{
    render_messages_pretty, render_messages_yaml, render_request_pretty, render_request_yaml,
    render_responses_request_pretty,
};
pub use request_options::{OutputCapField, RequestOptions};
pub use reservation::{
    OutputCapRejection, Reservation, ReservationState, UNKNOWN_OUTPUT_CAP, is_window_overflow,
    output_cap_rejection, reservation_state,
};
pub use responses::encode_responses;
pub use stages::{StageRecorder, StageSink};
pub use thinking::{ThinkingOff, ThinkingSupport, ToolChoice, check_effort_thinking};

use assembler::Assembler;
use thinking::{OutputConfig, Thinking};
