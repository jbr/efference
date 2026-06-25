//! The Anthropic Messages API wire — request/response shapes and the HTTP call.
//!
//! Streaming. The request `messages` are produced by [`crate::project::project`];
//! the response arrives as a Server-Sent Events stream and is reassembled by
//! [`Assembler`] back into a single [`Payload::Assistant`], so the result folds
//! straight into the log — this is where the substrate and the transport meet.
//! Reassembly is a pure fold over the event sequence (DESIGN §8: streaming wire);
//! the `Assembler` is unit-tested without a live stream.

use futures_lite::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trillium_client::Client as HttpClient;
use trillium_client::sse::SseErrorKind;
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
    /// never toggles the tools-cache tier (DESIGN §3). Their *content* must be
    /// byte-stable — `Bash::definition()` builds them from a [`Value`], whose
    /// keys serialize sorted, so it is.
    tools: &'a [Value],
    tool_choice: ToolChoice,
    messages: &'a [ApiMessage],
    /// Always `true` — we consume the response as SSE (DESIGN §8). Set once and
    /// never varied; a delivery flag, not part of the cached prefix (DESIGN §3),
    /// so it never invalidates the cache and isn't covered by the wire-bytes pin
    /// (which guards `project()` output, not this request envelope).
    stream: bool,
}

/// The `tool_choice` field. Phase 1+ always sets `disable_parallel_tool_use` so
/// the model emits **at most one** `tool_use` per turn (DESIGN §8) — set once and
/// never varied, so it never invalidates the cache. `auto()` lets the model
/// decide whether to call a tool; `force(name)` makes it call exactly that one
/// (the fork's `retain`, §4).
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

/// Carved out until the thinking slice (DESIGN §10): we recognize streamed
/// thinking / `redacted_thinking` blocks and their deltas and reject them with
/// this precise error rather than letting them fall into the generic
/// unknown-type arm. Storing thinking needs a `ContentBlock` variant, the
/// mandatory same-turn round-trip on the wire (which moves the byte-stability
/// pin), and a resolution for the fork's forced `tool_choice` — its own slice.
/// Until then, streaming against a thinking-enabled model fails loudly; dev runs
/// on Haiku, which emits none.
const THINKING_UNSUPPORTED: &str = "streamed a thinking/redacted_thinking block — \
    extended thinking is not yet supported (DESIGN §10); develop on Haiku until \
    the thinking slice lands";

/// One content block being reassembled from the stream, keyed by its `index`.
enum BlockAccum {
    Text(String),
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
///   (`output_tokens: 1`) — see DESIGN §3.
/// - **Thinking carved out, not built** (see [`THINKING_UNSUPPORTED`]).
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
    /// `event:` field; `data` is the raw JSON payload.
    fn apply(&mut self, event_type: Option<&str>, data: &str) -> Result<(), Error> {
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
                    "thinking" | "redacted_thinking" => return Err(THINKING_UNSUPPORTED.into()),
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
                let kind = delta.get("type").and_then(Value::as_str).unwrap_or_default();
                let block = self.blocks.get_mut(index).ok_or_else(|| -> Error {
                    format!("content_block_delta for unknown index {index}").into()
                })?;
                match (kind, block) {
                    ("text_delta", BlockAccum::Text(buf)) => {
                        buf.push_str(delta.get("text").and_then(Value::as_str).unwrap_or_default());
                    }
                    ("input_json_delta", BlockAccum::ToolUse { partial_json, .. }) => {
                        partial_json.push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    ("thinking_delta" | "signature_delta", _) => {
                        return Err(THINKING_UNSUPPORTED.into());
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
        Ok(())
    }

    /// Collapse the accumulated blocks into the assistant turn. Tool inputs parse
    /// here, after all fragments have arrived (an empty input → `{}`).
    fn finish(self) -> Result<Completion, Error> {
        let mut text = Vec::new();
        let mut calls = Vec::new();
        for block in self.blocks {
            match block {
                BlockAccum::Text(t) => text.push(ContentBlock::Text(t)),
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
            assistant: Payload::Assistant { text, calls },
            model: self.model,
            stop_reason: self.stop_reason,
            usage: self.usage,
        })
    }
}

/// A thin Anthropic Messages client: holds the HTTP stack, key, model, and the
/// stable system prompt. The system is byte-stable across the session, so it
/// stays in the cached prefix (DESIGN §3); the fork (§4) shares the same client
/// and thus the same system, so its prefix matches and hits cache.
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
    /// and fold the SSE event stream back into a [`Completion`].
    ///
    /// `into_sse` *is* the execution — the conn must not be awaited first (that
    /// is `SseErrorKind::AlreadyExecuted`). On a non-2xx it returns
    /// `SseErrorKind::Status` carrying the code but **not** the body (a known
    /// trillium-client limitation, filed in its backlog); we accept that lossy
    /// path here, since pre-stream non-2xx is mostly dev-time request bugs and
    /// operational overload arrives as an in-stream `error` event with its full
    /// body intact.
    pub async fn complete(
        &self,
        messages: &[ApiMessage],
        tools: &[Value],
        tool_choice: ToolChoice,
    ) -> Result<Completion, Error> {
        let request = Request {
            model: &self.model,
            max_tokens: MAX_TOKENS,
            system: &self.system,
            tools,
            tool_choice,
            messages,
            stream: true,
        };
        let body = serde_json::to_string(&request)?;

        let mut events = self
            .http
            .post(MESSAGES_ENDPOINT)
            .with_request_header("x-api-key", self.api_key.clone())
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .with_request_header("content-type", "application/json")
            .with_body(body)
            .into_sse()
            .await
            .map_err(|e| -> Error {
                match &e.kind {
                    SseErrorKind::Status(status) => {
                        format!("Anthropic API returned {status} (no body on the streaming path)")
                            .into()
                    }
                    other => format!("Anthropic SSE request failed: {other}").into(),
                }
            })?;

        let mut assembler = Assembler::new();
        while let Some(event) = events.next().await {
            let event = event?;
            assembler.apply(event.event_type(), event.data())?;
        }
        assembler.finish()
    }

    /// A cache-keepalive ping (DESIGN §3): a **non-streaming** `max_tokens: 0`
    /// round-trip over the same prefix a real turn would send. It READS the cached
    /// prefix — refreshing its (sliding) TTL — and generates nothing, so the
    /// (empty) assistant turn is discarded and the log is never touched. Returns
    /// only the `usage`, which the daemon folds into the §3 cache telemetry.
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
            Payload::Assistant { text, calls } => {
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
            ("message_start", r#"{"message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"now"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#),
        ])
        .unwrap();
        match completion.assistant {
            Payload::Assistant { calls, .. } => {
                assert_eq!(calls[0].input, serde_json::json!({}));
            }
            other => panic!("expected Payload::Assistant, got {other:?}"),
        }
    }

    /// A streamed thinking block is rejected with a precise, labeled error — the
    /// carved-out seam for the deferred thinking slice (DESIGN §10), not a silent
    /// drop.
    #[test]
    fn rejects_thinking_block_loudly() {
        let err = feed(&[
            ("message_start", r#"{"message":{"model":"claude-opus-4-8","usage":{"input_tokens":5,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("thinking"), "{err}");
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
}
