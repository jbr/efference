//! The Anthropic Messages API wire — request/response shapes and the HTTP call.
//!
//! Streaming. The request `messages` are produced by [`crate::project::project`];
//! the response arrives as a Server-Sent Events stream and is reassembled by
//! [`Assembler`] back into a single [`Payload::Assistant`], so the result folds
//! straight into the log — this is where the substrate and the transport meet.
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

const MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Response-token ceiling per request. 1024 truncated real final answers in a
/// live run (the model's synthesis was cut mid-sentence); 4096 gives the agent
/// room to conclude while staying bounded. A `Client` field can replace this when
/// per-call control becomes load-bearing.
const MAX_TOKENS: u32 = 4096;

/// Boxed error for the Phase 0 transport. A typed error enum can replace this
/// once error handling becomes load-bearing; for now non-2xx responses carry
/// the raw body in the message so failures are legible.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "str::is_empty")]
    system: &'a str,
    /// Tool definitions. Always sent (Phase 0 always has bash), so the request
    /// never toggles the tools-cache tier (docs/substrate.md). Their *content* must be
    /// byte-stable — `Bash::definition()` builds them from a [`Value`], whose
    /// keys serialize sorted, so it is.
    tools: &'a [Value],
    tool_choice: ToolChoice,
    /// Extended thinking (docs/transport.md, backlog/thinking-blocks.md). Rides on
    /// `tool_choice`: **adaptive** on the main agent turn (`tool_choice: auto`), **disabled**
    /// whenever the choice is forced — because forced `tool_choice` is incompatible with
    /// thinking (400), and the fork forces its distiller. Keepalive is disabled independently
    /// (`max_tokens: 0` rejects `enabled`; adaptive there is untested). Set once per call-type
    /// and never flipped turn-to-turn, so it stays out of the cached prefix's way (toggling
    /// thinking keeps the tools+system tier valid; docs/substrate.md). (`disabled` 400s on
    /// Fable 5, which can't disable thinking — not a serve target yet.)
    thinking: Thinking,
    messages: &'a [ApiMessage],
    /// Always `true` — we consume the response as SSE (docs/transport.md). Set once and
    /// never varied; a delivery flag, not part of the cached prefix (docs/substrate.md),
    /// so it never invalidates the cache and isn't covered by the wire-bytes pin
    /// (which guards `project()` output, not this request envelope).
    stream: bool,
}

/// The `thinking` field (docs/transport.md, backlog/thinking-blocks.md). `Adaptive` lets the
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
    /// Thinking rides on `tool_choice`: forced choice ⟺ no thinking (the API rejects the
    /// combination). Auto → adaptive; any forced choice → disabled. Encoding it here makes the
    /// illegal pairing unrepresentable (backlog/thinking-blocks.md).
    fn for_tool_choice(tool_choice: &ToolChoice) -> Self {
        match tool_choice.kind {
            "auto" => THINKING_ADAPTIVE,
            _ => THINKING_DISABLED,
        }
    }
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
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
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
    /// An extended-thinking block (docs/transport.md, backlog/thinking-blocks.md): `text`
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
///   (`output_tokens: 1`) — see docs/substrate.md.
/// - **Thinking assembled** into leading `ContentBlock::Thinking` / `RedactedThinking`
///   (docs/transport.md, backlog/thinking-blocks.md), signatures preserved for replay.
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
    /// `event:` field; `data` is the raw JSON payload. Returns the prose fragment a
    /// `text_delta` contributed (if any), so [`Client::complete`] can forward it to
    /// a live sink without re-parsing — every other event yields `None`.
    fn apply(&mut self, event_type: Option<&str>, data: &str) -> Result<Option<String>, Error> {
        let mut streamed = None;
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
                        streamed = Some(frag.to_string());
                    }
                    ("input_json_delta", BlockAccum::ToolUse { partial_json, .. }) => {
                        partial_json.push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    ("thinking_delta", BlockAccum::Thinking { text, .. }) => {
                        text.push_str(
                            delta
                                .get("thinking")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
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
            Some("error") => return Err(format!("Anthropic stream error: {data}").into()),
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

/// A thin Anthropic Messages client: holds the HTTP stack, key, model, and the
/// stable system prompt. The system is byte-stable across the session, so it
/// stays in the cached prefix (docs/substrate.md); the fork (docs/redaction-fork.md)
/// shares the same client and thus the same system, so its prefix matches and hits cache.
pub struct Client {
    http: HttpClient,
    api_key: String,
    model: String,
    system: String,
}

impl Client {
    pub fn new(api_key: String, model: impl Into<String>, system: impl Into<String>) -> Self {
        Self {
            // TLS over the tokio TCP connector; ALPN negotiates HTTP/2 (no h3
            // advertised by Anthropic/Cloudflare as of 2026-06). See main.rs.
            http: HttpClient::new(RustlsConfig::<ClientConfig>::default()),
            api_key,
            model: model.into(),
            system: system.into(),
        }
    }

    /// One streaming round-trip: serialize `messages`, POST with `stream: true`,
    /// and fold the SSE event stream back into a [`Completion`]. Each streamed prose
    /// fragment is handed to `on_text` as it arrives — the transport-only live sink
    /// (the daemon publishes it to `GET /stream`); pass a no-op `|_| {}` when nothing
    /// is listening (e.g. the fork peek, whose triage tokens must never surface).
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
        mut on_text: impl FnMut(&str),
    ) -> Result<Completion, Error> {
        let request = Request {
            model: &self.model,
            max_tokens: MAX_TOKENS,
            system: &self.system,
            tools,
            tool_choice,
            thinking: Thinking::for_tool_choice(&tool_choice),
            messages,
            stream: true,
        };
        let body = serde_json::to_string(&request)?;
        log::debug!(
            "POST {MESSAGES_ENDPOINT}: {} messages, {} request bytes",
            messages.len(),
            body.len()
        );

        let mut events = match self
            .http
            .post(MESSAGES_ENDPOINT)
            .with_request_header("x-api-key", self.api_key.clone())
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
            if let Some(fragment) = assembler.apply(event.event_type(), event.data())? {
                on_text(&fragment);
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
            format!("Anthropic API returned {status}: {body}")
        } else {
            format!("Anthropic SSE request failed: {}", e.kind)
        };
        log::error!("{message}");
        message.into()
    }

    /// A cache-keepalive ping (docs/transport.md): a **non-streaming** `max_tokens: 0`
    /// round-trip over the same prefix a real turn would send. It READS the cached
    /// prefix — refreshing its (sliding) TTL — and generates nothing, so the
    /// (empty) assistant turn is discarded and the log is never touched. Returns
    /// only the `usage`, which the daemon folds into the cache telemetry (docs/substrate.md).
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
            .post(MESSAGES_ENDPOINT)
            .with_request_header("x-api-key", self.api_key.clone())
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
        thinking: Thinking::for_tool_choice(&tool_choice),
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
        thinking: Thinking::for_tool_choice(&tool_choice),
        messages,
        stream: true,
    };
    serde_json::to_value(&request).expect("request envelope serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(events: &[(&str, &str)]) -> Result<Completion, Error> {
        let mut a = Assembler::new();
        for (ty, data) in events {
            a.apply(Some(ty), data)?;
        }
        a.finish()
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
    /// (docs/transport.md, backlog/thinking-blocks.md): `thinking_delta` chunks concatenate,
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
