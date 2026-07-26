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

/// The default upstream: Anthropic's own Messages endpoint. Used when neither
/// `EFFERENCE_ENDPOINT` nor `EFFERENCE_API_KEY` is set, so a bare `ANTHROPIC_API_KEY`
/// keeps working exactly as it did (docs/transport.md).
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// The `anthropic-version` header value. This is part of the Anthropic Messages
/// *wire dialect*, not a provider identity — OpenRouter's `/api/v1/messages` skin
/// speaks the same dialect and tolerates the header — so it rides every request
/// regardless of which `Endpoint` is targeted (docs/transport.md).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The provider endpoint (docs/transport.md). The request *body* is always the
/// Anthropic Messages wire — `project(log)` output — because that shape is what
/// every supported provider serves (Anthropic directly; OpenRouter via its
/// `/api/v1/messages` skin). Switching providers therefore only changes **where**
/// the POST goes and **how** it authenticates, so those are the only two things
/// this carries. Resolved once from the environment at boot and shared by the
/// agent [`Client`] and the fork's spotter client, so both hit the same endpoint
/// over the same cached prefix.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Full messages URL, e.g. `https://api.anthropic.com/v1/messages` or
    /// `https://openrouter.ai/api/v1/messages`.
    pub url: String,
    /// The auth header **name** — `x-api-key` for Anthropic, `authorization` for
    /// OpenRouter's `Bearer` scheme. A runtime `String` (not a `&'static str`) so
    /// the provider, not the code, chooses it.
    pub auth_header: String,
    /// The auth header **value**, already assembled — the raw key for `x-api-key`,
    /// or `Bearer sk-or-...` once the scheme prefix is prepended (see [`from_env`]).
    ///
    /// [`from_env`]: Self::from_env
    pub auth_value: String,
}

impl Endpoint {
    /// Resolve the endpoint from the environment. Every knob is optional, so an
    /// existing `ANTHROPIC_API_KEY`-only setup is entirely unchanged:
    ///
    /// - `EFFERENCE_ENDPOINT` — the full messages URL. Defaults to Anthropic's.
    /// - `EFFERENCE_API_KEY` — the **raw secret**, nothing else. Kept alone (not
    ///   `header=value`) so it can be a *whole-value* 1Password reference: `op run`
    ///   only substitutes a reference that is the entire value, never one embedded
    ///   mid-string. The header **name** and any scheme **prefix** are separate:
    /// - `EFFERENCE_AUTH_HEADER` — the auth header name. Defaults to `x-api-key`.
    /// - `EFFERENCE_AUTH_PREFIX` — a literal prepended to the secret to form the
    ///   header value. Defaults to empty; set `"Bearer "` (trailing space included)
    ///   for OpenRouter's `authorization: Bearer <key>`. Splitting the prefix off
    ///   keeps the secret a whole value, so `EFFERENCE_AUTH_PREFIX="Bearer "` +
    ///   `EFFERENCE_API_KEY="op://…"` resolves cleanly under `op run`.
    ///
    /// All provider knowledge thus stays in the environment, not the code. When
    /// `EFFERENCE_API_KEY` is absent, falls back to `ANTHROPIC_API_KEY` sent bare as
    /// `x-api-key` (the historical behavior); the `EFFERENCE_AUTH_*` knobs apply
    /// only to the `EFFERENCE_API_KEY` path.
    pub fn from_env() -> Result<Self, Error> {
        Self::resolve(
            std::env::var("EFFERENCE_ENDPOINT").ok(),
            std::env::var("EFFERENCE_API_KEY").ok(),
            std::env::var("EFFERENCE_AUTH_HEADER").ok(),
            std::env::var("EFFERENCE_AUTH_PREFIX").ok(),
            std::env::var("ANTHROPIC_API_KEY").ok(),
        )
    }

    /// The pure core of [`Self::from_env`], factored out so the precedence and the
    /// header/prefix/secret assembly are unit-testable without touching process env.
    fn resolve(
        endpoint: Option<String>,
        api_key: Option<String>,
        auth_header: Option<String>,
        auth_prefix: Option<String>,
        anthropic_key: Option<String>,
    ) -> Result<Self, Error> {
        let url = endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        match (api_key, anthropic_key) {
            // The configurable path: raw secret + optional header name + optional prefix.
            (Some(key), _) => {
                let auth_header = auth_header
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| "x-api-key".to_string());
                let auth_value = format!("{}{}", auth_prefix.unwrap_or_default(), key);
                Ok(Self {
                    url,
                    auth_header,
                    auth_value,
                })
            }
            // Backward-compat: a bare `ANTHROPIC_API_KEY` is always `x-api-key: <key>`,
            // ignoring the `EFFERENCE_AUTH_*` knobs (those belong to the path above).
            (None, Some(anthropic)) => Ok(Self {
                url,
                auth_header: "x-api-key".to_string(),
                auth_value: anthropic,
            }),
            (None, None) => Err("no API credentials: set EFFERENCE_API_KEY (plus \
                                 EFFERENCE_ENDPOINT / EFFERENCE_AUTH_HEADER / \
                                 EFFERENCE_AUTH_PREFIX for a non-Anthropic provider), or \
                                 ANTHROPIC_API_KEY for the default Anthropic endpoint"
                .into()),
        }
    }
}

/// Response-token ceiling per request. 1024 truncated real final answers in a
/// live run (the model's synthesis was cut mid-sentence); 4096 gives the agent
/// room to conclude while staying bounded. A `Client` field can replace this when
/// per-call control is needed.
const MAX_TOKENS: u32 = 4096;

/// Boxed error for the Phase 0 transport. A typed error enum can replace this
/// once error handling needs structure; for now non-2xx responses carry
/// the raw body in the message so failures are legible.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

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

/// The `thinking` field (docs/transport.md, docs/transport.md). `Adaptive` lets the
/// model decide when/how much to think and interleaves it across turns; `display: "summarized"`
/// captures a readable model-authored summary into the log (same token cost as omitted). The
/// serde tag renders `{"type":"adaptive","display":"summarized"}` / `{"type":"disabled"}`
/// exactly.
#[derive(Serialize, Clone, Copy)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Thinking {
    Disabled,
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
    },
}

const THINKING_DISABLED: Thinking = Thinking::Disabled;
const THINKING_ADAPTIVE: Thinking = Thinking::Adaptive {
    display: Some("summarized"),
};

impl Thinking {
    /// Thinking rides on `tool_choice` and the model: forced choice ⟺ no thinking (the API
    /// rejects the combination), and a model that can't run adaptive thinking (e.g. Haiku
    /// 4.5) never gets it even on `auto`. Encoding both here makes the illegal pairings
    /// unrepresentable (docs/transport.md).
    fn for_request(model: &str, tool_choice: &ToolChoice) -> Self {
        match tool_choice.kind {
            "auto" if supports_adaptive_thinking(model) => THINKING_ADAPTIVE,
            _ => THINKING_DISABLED,
        }
    }
}

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

/// Whether to send `thinking: {type:"adaptive"}` for `model` (docs/transport.md,
/// docs/transport.md). An explicit `EFFERENCE_THINKING` wins; otherwise this
/// is an **allowlist**, not "everything but haiku": most non-Anthropic models (reached
/// via OpenRouter) don't accept the Anthropic `thinking` field at all, so the safe
/// default is off unless the model is a recognized adaptive-capable Claude tier
/// (Sonnet/Opus/Fable — **not** Haiku, which takes `enabled`+`budget_tokens` only).
/// Matched by substring so both bare ids (`claude-opus-4-8`) and provider-scoped slugs
/// (`anthropic/claude-opus-4.1`) match. There is no portable capability query for this
/// — OpenRouter's `supported_parameters` describes its normalized `reasoning` interface,
/// not the Anthropic `thinking` block — so recognition lives here; override per session
/// with `EFFERENCE_THINKING=adaptive|off`.
fn supports_adaptive_thinking(model: &str) -> bool {
    THINKING_OVERRIDE.unwrap_or_else(|| is_recognized_adaptive_claude(model))
}

/// The allowlist behind [`supports_adaptive_thinking`]'s `auto` default: a Claude
/// family model (direct id or `anthropic/…` slug) that isn't Haiku. Everything
/// foreign (`openai/…`, `google/…`, …) falls outside it and thus gets no `thinking`.
///
/// Adaptive thinking (`{type:"adaptive"}`) is a Claude **4.6+** feature; pre-4.6
/// tiers (Opus 4.5/4.1, Sonnet 4.5/4, …) speak the older `{type:"enabled",
/// budget_tokens}` API and **400 on adaptive**. This substring rule doesn't encode
/// that version floor — parsing a version out of both `claude-opus-4-8` and
/// `anthropic/claude-opus-4.1` is its own fragile problem — so it assumes the
/// current Claude generation, which is what efference actually serves (default
/// `claude-opus-4-8`). efference can't express budgeted thinking anyway (the
/// [`Thinking`] enum is Adaptive/Disabled only), so a pre-4.6 Claude wants
/// thinking off: set `EFFERENCE_THINKING=off` for that non-target case.
fn is_recognized_adaptive_claude(model: &str) -> bool {
    (model.contains("claude") || model.contains("anthropic")) && !model.contains("haiku")
}

/// The `tool_choice` field. Phase 1+ always sets `disable_parallel_tool_use` so
/// the model emits **at most one** `tool_use` per turn (docs/README.md) — set once and
/// never varied, so it never invalidates the cache. `auto()` lets the model
/// decide whether to call a tool; `force(name)` makes it call exactly that one
/// (the fork's `retain`, docs/redaction-fork.md).
#[derive(Serialize, Clone, Copy)]
pub struct ToolChoice {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'static str>,
    disable_parallel_tool_use: bool,
}

impl ToolChoice {
    pub fn auto() -> Self {
        Self {
            kind: "auto",
            name: None,
            disable_parallel_tool_use: true,
        }
    }

    pub fn force(name: &'static str) -> Self {
        Self {
            kind: "tool",
            name: Some(name),
            disable_parallel_tool_use: true,
        }
    }
}

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
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

/// One content block being reassembled from the stream, keyed by its `index`.
enum BlockAccum {
    Text(String),
    /// An extended-thinking block (docs/transport.md, docs/transport.md): `text`
    /// accumulates `thinking_delta` chunks, `signature` arrives whole in a `signature_delta`
    /// near the block's end. The signature must be preserved verbatim for replay.
    Thinking {
        text: String,
        signature: String,
    },
    /// A safety-encrypted thinking block — opaque `data`, no text or signature.
    Redacted {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        /// Concatenated `input_json_delta.partial_json` chunks; the API splits a
        /// tool input across arbitrary byte boundaries, so we accumulate the raw
        /// fragments and parse the whole once the block stops.
        partial_json: String,
    },
}

/// What a single SSE event contributed to the *live* view, if anything
/// (backlog/dogfooding-friction.md — the invisible-generation-phase defect). The assembler folds
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

/// Reassembles a streamed SSE event sequence into a single [`Completion`].
///
/// Pure over the event sequence: [`Assembler::apply`] takes one event's `event:`
/// type and `data:` payload, so it is unit-testable without a live stream —
/// exactly as the old non-streaming `Response::into_completion` was. Invariants
/// carried over from that path:
///
/// - **Loud on unknown.** An unrecognized event, block, or delta type is a hard
///   error, never silently dropped (mirrors the old enum's deliberate parse
///   failure on unmodelled block types).
/// - **Usage merge.** `input_tokens` + `cache_*` come from `message_start`; the
///   final `output_tokens` from `message_delta`. Reading only one event would
///   regress the cache-gate metrics to the `message_start` placeholder
///   (`output_tokens: 1`) — see docs/log-and-projection.md.
/// - **Thinking assembled** into leading `ContentBlock::Thinking` / `RedactedThinking`
///   (docs/transport.md, docs/transport.md), signatures preserved for replay.
struct Assembler {
    /// Indexed by content-block `index`; pushed in order as blocks open.
    blocks: Vec<BlockAccum>,
    model: String,
    stop_reason: Option<String>,
    usage: Usage,
}

impl Assembler {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            model: String::new(),
            stop_reason: None,
            usage: Usage::default(),
        }
    }

    /// Fold one SSE event into the running state. `event_type` is the SSE
    /// `event:` field; `data` is the raw JSON payload. Returns what this event contributed to the
    /// *live* view as a [`Streamed`] (if anything), so [`Client::complete`] can forward it to a
    /// live sink without re-parsing — every event that carries no live signal yields `None`.
    fn apply(&mut self, event_type: Option<&str>, data: &str) -> Result<Option<Streamed>, Error> {
        let mut streamed = None;
        // The OpenAI/OpenRouter stream-end sentinel (docs/transport.md: providers).
        // Anthropic closes the stream after `message_stop`; OpenRouter's `/api/v1/messages`
        // skin appends a trailing `data: [DONE]` (labeled `event: data`) as OpenAI's wire
        // does. It carries no content — recognize it by its *payload*, since the event
        // label varies by provider, and treat it as terminal rather than tripping
        // loud-on-unknown below.
        if data.trim() == "[DONE]" {
            return Ok(None);
        }
        match event_type {
            Some("message_start") => {
                #[derive(Deserialize)]
                struct Env {
                    message: Msg,
                }
                #[derive(Deserialize)]
                struct Msg {
                    model: String,
                    usage: Usage,
                }
                let Env { message } = serde_json::from_str(data)?;
                self.model = message.model;
                // input + cache_* land here; output_tokens is the initial
                // placeholder, overwritten by message_delta.
                self.usage = message.usage;
            }
            Some("content_block_start") => {
                #[derive(Deserialize)]
                struct Env {
                    index: usize,
                    content_block: Value,
                }
                let Env {
                    index,
                    content_block,
                } = serde_json::from_str(data)?;
                if index != self.blocks.len() {
                    return Err(format!(
                        "out-of-order content_block_start: index {index}, have {}",
                        self.blocks.len()
                    )
                    .into());
                }
                let field = |k| content_block.get(k).and_then(Value::as_str);
                let block = match field("type").unwrap_or_default() {
                    "text" => BlockAccum::Text(field("text").unwrap_or_default().to_string()),
                    "tool_use" => BlockAccum::ToolUse {
                        id: field("id")
                            .ok_or_else(|| -> Error { "tool_use block missing id".into() })?
                            .to_string(),
                        name: field("name")
                            .ok_or_else(|| -> Error { "tool_use block missing name".into() })?
                            .to_string(),
                        partial_json: String::new(),
                    },
                    // Thinking leads the turn; `thinking`/`signature` accumulate via deltas
                    // (docs/transport.md). `content_block_start` may already carry an initial
                    // `thinking` string, so seed from it.
                    "thinking" => BlockAccum::Thinking {
                        text: field("thinking").unwrap_or_default().to_string(),
                        signature: field("signature").unwrap_or_default().to_string(),
                    },
                    "redacted_thinking" => BlockAccum::Redacted {
                        data: field("data").unwrap_or_default().to_string(),
                    },
                    other => return Err(format!("unknown content_block type {other:?}").into()),
                };
                // Name a tool call the moment it opens, before its arguments stream: a large input
                // can take seconds to arrive, and "calling `bash`…" is the difference between a
                // visible phase and apparent silence (backlog/dogfooding-friction.md).
                if let BlockAccum::ToolUse { name, .. } = &block {
                    streamed = Some(Streamed::ToolStart { name: name.clone() });
                }
                self.blocks.push(block);
            }
            Some("content_block_delta") => {
                #[derive(Deserialize)]
                struct Env {
                    index: usize,
                    delta: Value,
                }
                let Env { index, delta } = serde_json::from_str(data)?;
                let kind = delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let block = self.blocks.get_mut(index).ok_or_else(|| -> Error {
                    format!("content_block_delta for unknown index {index}").into()
                })?;
                match (kind, block) {
                    ("text_delta", BlockAccum::Text(buf)) => {
                        let frag = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        buf.push_str(frag);
                        streamed = Some(Streamed::Text(frag.to_string()));
                    }
                    ("input_json_delta", BlockAccum::ToolUse { partial_json, .. }) => {
                        let frag = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        partial_json.push_str(frag);
                        streamed = Some(Streamed::ToolInput(frag.to_string()));
                    }
                    ("thinking_delta", BlockAccum::Thinking { text, .. }) => {
                        let frag = delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        text.push_str(frag);
                        streamed = Some(Streamed::Thinking(frag.to_string()));
                    }
                    ("signature_delta", BlockAccum::Thinking { signature, .. }) => {
                        signature.push_str(
                            delta
                                .get("signature")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    (other, _) => {
                        return Err(
                            format!("unexpected delta type {other:?} at index {index}").into()
                        );
                    }
                }
            }
            // Block boundary: nothing to finalize incrementally (tool inputs are
            // parsed in `finish`, after every fragment has accumulated).
            Some("content_block_stop") => {}
            Some("message_delta") => {
                #[derive(Deserialize)]
                struct Env {
                    delta: Delta,
                    usage: DeltaUsage,
                }
                #[derive(Deserialize)]
                struct Delta {
                    #[serde(default)]
                    stop_reason: Option<String>,
                }
                #[derive(Deserialize)]
                struct DeltaUsage {
                    #[serde(default, deserialize_with = "null_or_missing")]
                    output_tokens: u64,
                }
                let Env { delta, usage } = serde_json::from_str(data)?;
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
                self.usage.output_tokens = usage.output_tokens;
            }
            // Terminal; the body ends after it, so the stream closes on its own.
            Some("message_stop") => {}
            // Keepalive.
            Some("ping") => {}
            // An in-stream error (e.g. `overloaded_error` after a 200) carries a
            // full JSON body — surface it verbatim (the lossy path is only the
            // pre-stream non-2xx in `complete`, see the trillium backlog note).
            Some("error") => return Err(format!("upstream stream error: {data}").into()),
            other => return Err(format!("unknown SSE event {other:?}: {data}").into()),
        }
        Ok(streamed)
    }

    /// Collapse the accumulated blocks into the assistant turn. Tool inputs parse
    /// here, after all fragments have arrived (an empty input → `{}`).
    fn finish(self) -> Result<Completion, Error> {
        let mut thinking = Vec::new();
        let mut text = Vec::new();
        let mut calls = Vec::new();
        for block in self.blocks {
            match block {
                BlockAccum::Text(t) => text.push(ContentBlock::Text(t)),
                BlockAccum::Thinking { text: t, signature } => {
                    thinking.push(ContentBlock::Thinking { text: t, signature })
                }
                BlockAccum::Redacted { data } => {
                    thinking.push(ContentBlock::RedactedThinking { data })
                }
                BlockAccum::ToolUse {
                    id,
                    name,
                    partial_json,
                } => {
                    let input = if partial_json.trim().is_empty() {
                        Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(&partial_json)?
                    };
                    calls.push(ToolCall {
                        id: CallId(id),
                        name,
                        input,
                    });
                }
            }
        }
        Ok(Completion {
            assistant: Payload::Assistant {
                thinking,
                text,
                calls,
            },
            model: self.model,
            stop_reason: self.stop_reason,
            usage: self.usage,
        })
    }
}

/// A thin Anthropic Messages client: holds the HTTP stack, the [`Endpoint`] (URL +
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
            http: HttpClient::new(RustlsConfig::<ClientConfig>::default()),
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

/// Render the full request envelope as pretty JSON — the dev `wire` view (the
/// `efference wire` subcommand) of exactly what [`Client::complete`] POSTs for a
/// real turn: the same private [`Request`] shape, with `max_tokens`/`stream` set
/// as a streaming turn sends them. The transport headers (`x-api-key`,
/// `anthropic-version`) are request metadata, not part of the body the model
/// reads, so they're omitted. Reusing [`Request`] keeps this faithful by
/// construction: a wire-shape change can't drift the dev view out of sync.
///
/// Serializes the struct directly (not via [`request_value`]) so the top-level
/// field order is preserved — `serde_json::Value` would re-sort the keys, burying
/// `model`/`system` beneath the long `tools`/`messages` blocks.
pub fn render_request_pretty(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    messages: &[ApiMessage],
) -> Result<String, Error> {
    let request = Request {
        model,
        max_tokens: MAX_TOKENS,
        system,
        tools,
        tool_choice,
        thinking: Thinking::for_request(model, &tool_choice),
        messages,
        stream: true,
    };
    Ok(serde_json::to_string_pretty(&request)?)
}

/// Render the same envelope as [`render_request_pretty`] as **pseudo-YAML** (the
/// dev `wire --format yaml` view): a lossy, read-only rendering that unfolds
/// escaped/multi-line strings — tool-schema descriptions, the system prompt,
/// tool-result bodies — into indented blocks instead of `\n`-littered one-liners
/// (see [`crate::pretty`]). Top-level keys are emitted in a hand-picked, readable
/// order (scalars and `system` before the long `tools`/`messages` blocks) rather
/// than the map's sorted order; nested objects follow the sorted default.
pub fn render_request_yaml(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    messages: &[ApiMessage],
) -> Result<String, Error> {
    let value = request_value(model, system, tools, tool_choice, messages);
    let obj = value
        .as_object()
        .expect("request envelope serializes to a JSON object");
    let order = [
        "model",
        "max_tokens",
        "system",
        "tools",
        "tool_choice",
        "thinking",
        "messages",
    ];
    let mut lines = Vec::new();
    for key in order {
        if let Some(v) = obj.get(key) {
            lines.extend(crate::pretty::keyed(key, v));
        }
    }
    Ok(crate::pretty::join(lines))
}

/// Render just the projected `messages` array — the `wire --messages` view: what
/// [`crate::project::project`] produced (with the trailing cache breakpoint
/// placed), skipping the `system`/`tools[]` envelope entirely. Unlike
/// [`render_request_pretty`], this needs no model id, system prompt, or tool
/// list, so it works even when a session's config won't parse (nothing here
/// depends on it) — a lean, config-independent view of what the model sees.
pub fn render_messages_pretty(messages: &[ApiMessage]) -> Result<String, Error> {
    Ok(serde_json::to_string_pretty(messages)?)
}

/// Pseudo-YAML counterpart to [`render_messages_pretty`] (see [`crate::pretty`]
/// for the rendering trade-offs): the projected `messages` array, unfolded for
/// readability, with no `system`/`tools[]` envelope.
pub fn render_messages_yaml(messages: &[ApiMessage]) -> Result<String, Error> {
    let value = serde_json::to_value(messages)?;
    Ok(crate::pretty::to_pseudo_yaml(&value))
}

/// The request envelope as a [`Value`] — the shared source for both dev renderers
/// (`render_request_*`). Reuses the private [`Request`] shape, so a wire-shape
/// change can't drift the dev views out of sync with what `complete` POSTs.
fn request_value(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    messages: &[ApiMessage],
) -> Value {
    let request = Request {
        model,
        max_tokens: MAX_TOKENS,
        system,
        tools,
        tool_choice,
        thinking: Thinking::for_request(model, &tool_choice),
        messages,
        stream: true,
    };
    serde_json::to_value(&request).expect("request envelope serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare `ANTHROPIC_API_KEY` (no EFFERENCE_* knobs) resolves to today's
    /// behavior: Anthropic's endpoint, `x-api-key` auth. This is the
    /// backward-compat contract.
    #[test]
    fn endpoint_defaults_to_anthropic_from_bare_key() {
        let e = Endpoint::resolve(None, None, None, None, Some("sk-ant-xyz".into())).unwrap();
        assert_eq!(e.url, DEFAULT_ENDPOINT);
        assert_eq!(e.auth_header, "x-api-key");
        assert_eq!(e.auth_value, "sk-ant-xyz");
    }

    /// OpenRouter: a custom endpoint, `authorization` header, and a `Bearer ` scheme
    /// prefix — all from the environment, and each a *whole* value (so the secret can
    /// be an `op://…` reference `op run` resolves). No provider knowledge in the code.
    #[test]
    fn endpoint_openrouter_via_prefix_and_header() {
        let e = Endpoint::resolve(
            Some("https://openrouter.ai/api/v1/messages".into()),
            Some("sk-or-v1-abc".into()),
            Some("authorization".into()),
            Some("Bearer ".into()),
            None,
        )
        .unwrap();
        assert_eq!(e.url, "https://openrouter.ai/api/v1/messages");
        assert_eq!(e.auth_header, "authorization");
        assert_eq!(e.auth_value, "Bearer sk-or-v1-abc");
    }

    /// `EFFERENCE_API_KEY` with no header/prefix defaults to a bare `x-api-key`, and
    /// the key rides through verbatim (base64 `=` padding intact).
    #[test]
    fn endpoint_api_key_defaults_to_bare_x_api_key() {
        let e = Endpoint::resolve(None, Some("abc==".into()), None, None, None).unwrap();
        assert_eq!(e.auth_header, "x-api-key");
        assert_eq!(e.auth_value, "abc==");
    }

    /// `EFFERENCE_API_KEY` wins over a lingering `ANTHROPIC_API_KEY`.
    #[test]
    fn endpoint_api_key_overrides_anthropic_key() {
        let e =
            Endpoint::resolve(None, Some("z".into()), None, None, Some("sk-ant".into())).unwrap();
        assert_eq!(e.auth_header, "x-api-key");
        assert_eq!(e.auth_value, "z");
    }

    /// A blank `EFFERENCE_AUTH_HEADER` falls back to `x-api-key` rather than sending
    /// an empty header name.
    #[test]
    fn endpoint_blank_header_falls_back_to_x_api_key() {
        let e = Endpoint::resolve(None, Some("k".into()), Some("   ".into()), None, None).unwrap();
        assert_eq!(e.auth_header, "x-api-key");
        assert_eq!(e.auth_value, "k");
    }

    /// The `EFFERENCE_AUTH_*` knobs apply only to the `EFFERENCE_API_KEY` path — a
    /// stray prefix never mutates the `ANTHROPIC_API_KEY` fallback.
    #[test]
    fn endpoint_prefix_ignored_on_anthropic_fallback() {
        let e = Endpoint::resolve(
            None,
            None,
            None,
            Some("Bearer ".into()),
            Some("sk-ant".into()),
        )
        .unwrap();
        assert_eq!(e.auth_header, "x-api-key");
        assert_eq!(e.auth_value, "sk-ant");
    }

    /// No credentials at all is an error naming both ways to provide them.
    #[test]
    fn endpoint_errors_with_no_credentials() {
        assert!(Endpoint::resolve(None, None, None, None, None).is_err());
    }

    /// The `auto` thinking default is a Claude allowlist, not "everything but haiku":
    /// foreign models (reached via OpenRouter) never get an Anthropic `thinking` block,
    /// while direct ids and `anthropic/…` slugs do — except Haiku.
    #[test]
    fn adaptive_thinking_allowlist_is_claude_non_haiku() {
        assert!(is_recognized_adaptive_claude("claude-opus-4-8"));
        assert!(is_recognized_adaptive_claude("claude-sonnet-5"));
        assert!(is_recognized_adaptive_claude("anthropic/claude-opus-4.1"));
        assert!(is_recognized_adaptive_claude("claude-fable-5"));
        assert!(!is_recognized_adaptive_claude("claude-haiku-4-5-20251001"));
        assert!(!is_recognized_adaptive_claude("anthropic/claude-haiku-4.5"));
        assert!(!is_recognized_adaptive_claude("openai/gpt-5.6"));
        assert!(!is_recognized_adaptive_claude("google/gemini-2.5-pro"));
        assert!(!is_recognized_adaptive_claude("deepseek/deepseek-chat"));
    }

    fn feed(events: &[(&str, &str)]) -> Result<Completion, Error> {
        let mut a = Assembler::new();
        for (ty, data) in events {
            a.apply(Some(ty), data)?;
        }
        a.finish()
    }

    /// Everything the sink sees, in order — the live channel, as distinct from the assembled
    /// [`Completion`] that `feed` returns.
    fn live(events: &[(&str, &str)]) -> Vec<Streamed> {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        for (ty, data) in events {
            if let Some(s) = a.apply(Some(ty), data).unwrap() {
                out.push(s);
            }
        }
        out
    }

    /// **Every generation phase reaches the live sink**, not just prose
    /// (backlog/dogfooding-friction.md). The sink used to carry a bare text fragment, so a turn
    /// that thought and then emitted a tool call published *nothing* until prose arrived — and a
    /// turn with no prose at all published nothing, full stop, which is indistinguishable from a
    /// hung harness. The tool call is named at `content_block_start`, before its arguments,
    /// because a large input can take seconds to stream.
    #[test]
    fn every_phase_reaches_the_live_sink_not_just_prose() {
        let seen = live(&[
            (
                "message_start",
                r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":1,"output_tokens":1}}}"#,
            ),
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"weighing "}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"the options"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            ),
            ("content_block_stop", r#"{"index":0}"#),
            (
                "content_block_start",
                r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"ls\"}"}}"#,
            ),
            ("content_block_stop", r#"{"index":1}"#),
        ]);
        assert_eq!(
            seen,
            vec![
                Streamed::Thinking("weighing ".into()),
                Streamed::Thinking("the options".into()),
                // The call is named before a single argument byte arrives.
                Streamed::ToolStart {
                    name: "bash".into()
                },
                Streamed::ToolInput("{\"comm".into()),
                Streamed::ToolInput("and\":\"ls\"}".into()),
            ],
            "a thinking-then-tool-call turn must not stream silently"
        );
        // A signature delta carries no live signal — it's replay metadata, not progress.
        assert!(!seen.iter().any(|s| matches!(s, Streamed::Text(_))));
    }

    /// Prose still streams as before, and is typed as prose — the regression guard on the
    /// existing live view, which is what the sink carried before it was typed.
    #[test]
    fn prose_still_reaches_the_sink_as_text() {
        let seen = live(&[
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"text_delta","text":"let me "}}"#,
            ),
            (
                "content_block_delta",
                r#"{"index":0,"delta":{"type":"text_delta","text":"look"}}"#,
            ),
        ]);
        assert_eq!(
            seen,
            vec![
                Streamed::Text("let me ".into()),
                Streamed::Text("look".into())
            ]
        );
    }

    /// The path the bash tool depends on: a text block plus a tool call whose
    /// JSON input is **split across three `input_json_delta` fragments** — the
    /// reassembly that makes streaming non-trivial. Also pins the usage merge:
    /// `cache_read` from `message_start`, final `output_tokens` from
    /// `message_delta`.
    #[test]
    fn assembles_text_and_split_tool_use() {
        let completion = feed(&[
            ("message_start", r#"{"type":"message_start","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":9407}}}"#),
            ("ping", r#"{"type":"ping"}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"let me look"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash"}}"#),
            // One JSON object, three arbitrary byte splits.
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"l"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"s\"}"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();

        assert_eq!(completion.model, "claude-haiku-4-5");
        assert_eq!(completion.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(completion.usage.input_tokens, 3);
        assert_eq!(completion.usage.cache_read_input_tokens, 9407); // message_start
        assert_eq!(completion.usage.output_tokens, 4); // message_delta, not the placeholder 1

        match completion.assistant {
            Payload::Assistant {
                thinking,
                text,
                calls,
            } => {
                assert!(thinking.is_empty(), "no thinking blocks in this stream");
                assert_eq!(text, vec![ContentBlock::Text("let me look".into())]);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "bash");
                assert_eq!(calls[0].id.0, "toolu_9");
                assert_eq!(calls[0].input, serde_json::json!({ "command": "ls" }));
            }
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }

    /// OpenRouter's `/api/v1/messages` skin sends the `cache_*` token counts as an
    /// explicit `null` in every `message_start` (the upstream — here Together — has no
    /// caching to report), rather than omitting them as Anthropic does. Regression: a
    /// present `null` is not covered by `#[serde(default)]` and crashed the first
    /// OpenRouter turn with `invalid type: null, expected u64`. These are the exact
    /// bytes captured from `thinkingmachines/inkling`.
    #[test]
    fn openrouter_null_cache_usage_does_not_crash() {
        let completion = feed(&[
            ("message_start", r#"{"type":"message_start","message":{"id":"gen-1","type":"message","role":"assistant","container":null,"content":[],"model":"thinkingmachines/inkling","stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":null,"cache_read_input_tokens":null,"cache_creation":null,"inference_geo":null,"server_tool_use":null,"service_tier":null,"speed":"standard"},"provider":"Together"}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":17,"output_tokens":64,"cache_creation_input_tokens":null,"cache_read_input_tokens":0}}"#),
        ])
        .unwrap();
        assert_eq!(completion.model, "thinkingmachines/inkling");
        assert_eq!(completion.usage.input_tokens, 0);
        assert_eq!(completion.usage.cache_creation_input_tokens, 0); // null → 0
        assert_eq!(completion.usage.cache_read_input_tokens, 0); // null → 0
        assert_eq!(completion.usage.output_tokens, 64); // message_delta, non-null
    }

    /// A tool call with no `input_json_delta` fragments projects to `{}`, not a
    /// parse error.
    #[test]
    fn empty_tool_input_is_empty_object() {
        let completion = feed(&[
            (
                "message_start",
                r#"{"message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#,
            ),
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"now"}}"#,
            ),
            ("content_block_stop", r#"{"index":0}"#),
            (
                "message_delta",
                r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
            ),
        ])
        .unwrap();
        match completion.assistant {
            Payload::Assistant { calls, .. } => {
                assert_eq!(calls[0].input, serde_json::json!({}));
            }
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }

    /// A streamed thinking block is assembled into a leading `ContentBlock::Thinking`
    /// (docs/transport.md, docs/transport.md): `thinking_delta` chunks concatenate,
    /// the `signature_delta` is preserved verbatim (mandatory for replay), and the block leads
    /// the turn — before the text block that follows it in the stream.
    #[test]
    fn assembles_thinking_block_leading_the_turn() {
        let completion = feed(&[
            ("message_start", r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":5,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"weighing "}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"the options"}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"signature_delta","signature":"SiG=="}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("content_block_start", r#"{"index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"text_delta","text":"here's the answer"}}"#),
            ("content_block_stop", r#"{"index":1}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}"#),
        ])
        .unwrap();
        match completion.assistant {
            Payload::Assistant {
                thinking,
                text,
                calls,
            } => {
                assert_eq!(
                    thinking,
                    vec![ContentBlock::Thinking {
                        text: "weighing the options".into(),
                        signature: "SiG==".into(),
                    }]
                );
                assert_eq!(text, vec![ContentBlock::Text("here's the answer".into())]);
                assert!(calls.is_empty());
            }
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }

    /// A `redacted_thinking` block (opaque `data`, no text/signature) is preserved into a
    /// leading `ContentBlock::RedactedThinking` for verbatim replay.
    #[test]
    fn assembles_redacted_thinking_block() {
        let completion = feed(&[
            ("message_start", r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":5,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"redacted_thinking","data":"ENCRYPTED=="}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
        ])
        .unwrap();
        match completion.assistant {
            Payload::Assistant { thinking, .. } => assert_eq!(
                thinking,
                vec![ContentBlock::RedactedThinking {
                    data: "ENCRYPTED==".into()
                }]
            ),
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }

    /// An unknown event type is loud, never silently dropped.
    #[test]
    fn unknown_event_type_is_loud() {
        let err = feed(&[("surprise", "{}")]).unwrap_err();
        assert!(err.to_string().contains("surprise"), "{err}");
    }

    /// OpenRouter's skin terminates the stream with a trailing `[DONE]` sentinel
    /// (labeled `event: data`) as OpenAI's wire does; Anthropic never sends one.
    /// It must be recognized as terminal, not tripped as an unknown event — this is
    /// the second thing that crashed the first OpenRouter turn after the null usage.
    #[test]
    fn done_sentinel_is_terminal_not_unknown() {
        let completion = feed(&[
            ("message_start", r#"{"message":{"model":"thinkingmachines/inkling","usage":{"input_tokens":1,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"hi"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
            // The OpenRouter/OpenAI terminator — must not error.
            ("data", "[DONE]"),
        ])
        .unwrap();
        match completion.assistant {
            Payload::Assistant { text, .. } => {
                assert_eq!(text, vec![ContentBlock::Text("hi".into())]);
            }
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }

    /// An in-stream `error` event surfaces its full body (the path that keeps the
    /// useful overload/rate-limit detail the pre-stream non-2xx path loses).
    #[test]
    fn in_stream_error_surfaces_body() {
        let err = feed(&[(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )])
        .unwrap_err();
        assert!(err.to_string().contains("overloaded_error"), "{err}");
    }

    use crate::project::ApiBlock;

    fn sample_messages() -> Vec<ApiMessage> {
        vec![
            ApiMessage {
                role: crate::project::Role::User,
                content: vec![ApiBlock::Text {
                    text: "hello".into(),
                    cache_control: None,
                }],
            },
            ApiMessage {
                role: crate::project::Role::Assistant,
                content: vec![ApiBlock::Text {
                    text: "hi there".into(),
                    cache_control: None,
                }],
            },
        ]
    }

    /// `render_messages_pretty` (the `wire --messages` JSON view) is exactly the
    /// projected array — no `system`/`tools`/`model` envelope wrapped around it.
    #[test]
    fn render_messages_pretty_is_just_the_array() {
        let messages = sample_messages();
        let rendered = render_messages_pretty(&messages).unwrap();
        let value: Value = serde_json::from_str(&rendered).unwrap();
        assert!(value.is_array(), "expected a bare array, got {value}");
        assert_eq!(value, serde_json::to_value(&messages).unwrap());
        assert!(!rendered.contains("\"system\""));
        assert!(!rendered.contains("\"tools\""));
    }

    /// `render_messages_yaml` unfolds the same array as pseudo-YAML, with no
    /// envelope keys either.
    #[test]
    fn render_messages_yaml_is_just_the_array() {
        let messages = sample_messages();
        let rendered = render_messages_yaml(&messages).unwrap();
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("hi there"));
        assert!(!rendered.contains("system:"));
        assert!(!rendered.contains("tools:"));
    }
}
