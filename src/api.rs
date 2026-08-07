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

/// The last non-success status received from the upstream API, plus consecutive
/// occurrences of that status. Ephemeral transport diagnostics: never appended
/// to the log or projected (docs/transport.md).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct UpstreamFailure {
    pub status: u16,
    pub count: u32,
}

/// Shared state written by the HTTP client and read through the daemon's
/// `GET /state` view (docs/transport.md).
pub type UpstreamFailureState = std::sync::Arc<std::sync::Mutex<Option<UpstreamFailure>>>;

/// Construct an empty upstream-failure diagnostic cell.
pub fn upstream_failure_state() -> UpstreamFailureState {
    std::sync::Arc::new(std::sync::Mutex::new(None))
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
    /// `tool_choice` and the model: **adaptive** on the main agent turn (`tool_choice: auto`)
    /// when the model supports it, **disabled** whenever the choice is forced (forced
    /// `tool_choice` is incompatible with thinking — 400 — and the fork forces its distiller)
    /// or the model can't run adaptive thinking (Haiku 4.5; see `supports_adaptive_thinking`).
    /// Keepalive is disabled independently (`max_tokens: 0` rejects `enabled`; adaptive there
    /// is untested). Set once per call-type and never flipped turn-to-turn, so it stays out of
    /// the cached prefix's way (toggling thinking keeps the tools+system tier valid;
    /// docs/log-and-projection.md). (`disabled` 400s on Fable 5, which can't disable thinking — not a
    /// serve target yet.)
    thinking: Thinking,
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
/// to the model allowlist. Read once — env is process-stable and thinking must not
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
    pub usage: Usage,
}

impl Completion {
    /// This completion's [`RequestUsage`] — the per-request record the daemon persists.
    pub fn request_usage(&self) -> RequestUsage {
        RequestUsage {
            id: self.id.clone(),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Streamed {
    /// A fragment of assistant prose.
    Text(String),
    /// A fragment of extended-thinking text. Never part of the assistant's answer — a client
    /// showing this must mark it as thinking, not prose.
    Thinking(String),
    /// The model began emitting a tool call. Arrives at `content_block_start`, *before* the
    /// arguments stream, so a client can name the pending call immediately — the arguments alone
    /// can take many seconds to arrive for a large input.
    ToolStart { name: String },
    /// A fragment of a tool call's JSON input. Deliberately not parsed: it's a partial byte slice
    /// of a JSON document, so the only honest use is progress (that bytes are arriving), not
    /// content.
    ToolInput(String),
}

#[cfg(test)]
mod test;

mod assembler;
mod client;
mod endpoint;
mod provider;
mod render;
mod thinking;

pub use client::Client;
pub use endpoint::{Endpoint, sticky_session_id};
pub use provider::{ModelCapabilities, Provider};
pub use render::{
    render_messages_pretty, render_messages_yaml, render_request_pretty, render_request_yaml,
};
pub use thinking::ToolChoice;

use assembler::Assembler;
use thinking::Thinking;
