//! Reassembling an SSE event stream into one `Completion` (docs/transport.md).

use super::*;

/// One content block being reassembled from the stream, keyed by its `index`.
pub(super) enum BlockAccum {
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
pub(super) struct Assembler {
    /// Indexed by content-block `index`; pushed in order as blocks open.
    blocks: Vec<BlockAccum>,
    id: Option<String>,
    model: String,
    stop_reason: Option<String>,
    usage: Usage,
}

impl Assembler {
    pub(super) fn new() -> Self {
        Self {
            blocks: Vec::new(),
            id: None,
            model: String::new(),
            stop_reason: None,
            usage: Usage::default(),
        }
    }

    /// Fold one SSE event into the running state. `event_type` is the SSE
    /// `event:` field; `data` is the raw JSON payload. Returns what this event contributed to the
    /// *live* view as a [`Streamed`] (if anything), so [`Client::complete`] can forward it to a
    /// live sink without re-parsing — every event that carries no live signal yields `None`.
    pub(super) fn apply(
        &mut self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<Option<Streamed>, Error> {
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
                    #[serde(default)]
                    id: Option<String>,
                    model: String,
                    usage: Usage,
                }
                let Env { message } = serde_json::from_str(data)?;
                // The upstream response/generation id — persisted with the usage
                // record (docs/log-and-projection.md); lenient because it is
                // telemetry, not content.
                self.id = message.id;
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
                // visible phase and apparent silence (docs/transport.md).
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
    pub(super) fn finish(self) -> Result<Completion, Error> {
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
            id: self.id,
            model: self.model,
            stop_reason: self.stop_reason,
            usage: self.usage,
        })
    }
}
