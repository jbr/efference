//! Reassembling an SSE event stream into one `Completion` (docs/transport.md).

use super::*;

use std::collections::BTreeMap;

use crate::api::failure::{FailureClass, classify_body};
use crate::log::{FRAME_CAPTURE_CAP, StreamDiagnostics, UpstreamFailureRecord};

/// One content block being reassembled from the stream, keyed by its `index`.
pub(super) enum BlockAccum {
    Text(String),
    /// An extended-thinking block (docs/transport.md): `text` accumulates
    /// `thinking_delta` chunks, `signature` arrives whole in a `signature_delta`
    /// near the block's end. The signature must be preserved verbatim, because the
    /// API rejects a modified block on replay.
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
/// type and `data:` payload, so it is unit-testable without a live stream. The
/// three invariants it holds — loud-on-unknown, finalized-or-marked, and the
/// usage merge — are in docs/transport.md.
pub(super) struct Assembler {
    /// Indexed by content-block `index`; pushed in order as blocks open.
    blocks: Vec<BlockAccum>,
    id: Option<String>,
    model: String,
    stop_reason: Option<String>,
    usage: Usage,
    /// Whether the stream finalized: `message_stop` (Anthropic) or the `[DONE]`
    /// sentinel (OpenRouter) observed. A stream that closes without either died
    /// upstream — the completion assembles as a record-only partial
    /// (docs/transport.md), never a clean turn.
    finalized: bool,
    /// A mid-stream death recorded by [`Assembler::fail`]: an in-stream `error`
    /// event's body, a transport error, or an unparseable event. Carried into the
    /// completion's `upstream_failure` so the why survives to the log.
    failure: Option<String>,
    /// How `failure` should be retried, from the provider's naming of it
    /// (`api/failure.rs`).
    failure_class: FailureClass,
    /// The response the stream arrived on, as [`Assembler::observed_response`]
    /// recorded it. Only the empty-finalized path reads it (docs/transport.md).
    response: Option<ResponseMeta>,
    /// Decoded frames in arrival order, buffered up to [`FRAME_CAPTURE_CAP`].
    /// Buffered for every stream because whether this one is the empty stream is
    /// only decidable at [`Assembler::finish`]; kept in the completion only there.
    frames: String,
    /// Whether the cap stopped [`Self::frames`] short.
    frames_truncated: bool,
    /// The `message_start` event's `data` verbatim, subject to the same cap.
    message_start: Option<String>,
}

/// The response half of the evidence: what [`Client::complete`] read off the
/// executed conn once the stream opened (docs/transport.md).
pub(crate) struct ResponseMeta {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
}

impl Assembler {
    pub(super) fn new() -> Self {
        Self {
            blocks: Vec::new(),
            id: None,
            model: String::new(),
            stop_reason: None,
            usage: Usage::default(),
            finalized: false,
            failure: None,
            failure_class: FailureClass::Transient,
            response: None,
            frames: String::new(),
            frames_truncated: false,
            message_start: None,
        }
    }

    /// Record the response the stream opened on. Called once, before any event is
    /// applied; only the empty-finalized path puts it in the log
    /// (docs/transport.md).
    pub(super) fn observed_response(&mut self, response: ResponseMeta) {
        self.response = Some(response);
    }

    /// Buffer one decoded frame, up to [`FRAME_CAPTURE_CAP`] bytes of frames per
    /// stream. Past the cap the head is kept and `frames_truncated` says so — a
    /// stream large enough to hit it is not the stream this capture is for.
    fn capture_frame(&mut self, event_type: Option<&str>, data: &str) {
        if self.frames_truncated {
            return;
        }
        let frame = format!(
            "event: {}\ndata: {data}\n\n",
            event_type.unwrap_or("(none)")
        );
        if self.frames.len() + frame.len() > FRAME_CAPTURE_CAP {
            self.frames_truncated = true;
            return;
        }
        self.frames.push_str(&frame);
    }

    /// Record why the stream died mid-flight. The caller stops feeding events after
    /// this; [`Assembler::finish`] folds the message into the completion's
    /// `upstream_failure` instead of discarding the partial — whatever accumulated
    /// before the death is the only copy anywhere (docs/transport.md).
    pub(super) fn fail(&mut self, why: String) {
        self.failure = Some(why);
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
        // Every frame is buffered, terminator included: the evidence an empty
        // stream leaves is exactly the frames that arrived (docs/transport.md).
        self.capture_frame(event_type, data);
        // The OpenAI/OpenRouter stream-end sentinel (docs/transport.md: providers).
        // Anthropic closes the stream after `message_stop`; OpenRouter's `/api/v1/messages`
        // skin appends a trailing `data: [DONE]` (labeled `event: data`) as OpenAI's wire
        // does. It carries no content — recognize it by its *payload*, since the event
        // label varies by provider, and treat it as terminal rather than tripping
        // loud-on-unknown below.
        if data.trim() == "[DONE]" {
            self.finalized = true;
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
                // The body as received, kept whole under the same cap: it is the
                // one frame an empty stream is known to deliver, and any field
                // this struct does not model — a provider name, say — survives in
                // it (docs/transport.md).
                if data.len() <= FRAME_CAPTURE_CAP {
                    self.message_start = Some(data.to_string());
                }
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
                if let BlockAccum::ToolUse { id, name, .. } = &block {
                    streamed = Some(Streamed::ToolStart {
                        index,
                        id: id.clone(),
                        name: name.clone(),
                    });
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
                        streamed = Some(Streamed::Text {
                            index,
                            text: frag.to_string(),
                        });
                    }
                    ("input_json_delta", BlockAccum::ToolUse { partial_json, .. }) => {
                        let frag = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        partial_json.push_str(frag);
                        streamed = Some(Streamed::ToolInput {
                            index,
                            fragment: frag.to_string(),
                        });
                    }
                    ("thinking_delta", BlockAccum::Thinking { text, .. }) => {
                        let frag = delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        text.push_str(frag);
                        streamed = Some(Streamed::Thinking {
                            index,
                            text: frag.to_string(),
                        });
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
                    usage: Usage,
                }
                #[derive(Deserialize)]
                struct Delta {
                    #[serde(default)]
                    stop_reason: Option<String>,
                }
                let Env { delta, usage } = serde_json::from_str(data)?;
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
                self.usage.merge_max(&usage);
            }
            // Terminal; the body ends after it, so the stream closes on its own.
            Some("message_stop") => self.finalized = true,
            // Keepalive.
            Some("ping") => {}
            // An in-stream error (e.g. `overloaded_error` after a 200) carries a
            // full JSON body — surface it verbatim, so it reaches the completion's
            // `upstream_failure` and thus the log (docs/transport.md).
            Some("error") => {
                self.failure_class = classify_body(data);
                return Err(format!("upstream stream error: {data}").into());
            }
            other => return Err(format!("unknown SSE event {other:?}: {data}").into()),
        }
        Ok(streamed)
    }

    /// Collapse the accumulated blocks into the assistant turn. Tool inputs parse
    /// here, after all fragments have arrived (an empty input → `{}`).
    ///
    /// A stream that never finalized — no `message_stop` / `[DONE]`, or a death
    /// recorded via [`Assembler::fail`] — assembles into the same `Completion`, but
    /// with `upstream_failure` set on the payload: a record-only partial the loop
    /// appends for audit and retries, never a clean turn (docs/transport.md). A
    /// failure recorded *after* finalization (a transport error while draining the
    /// already-closed stream) does not unmake the completion: the provider stood
    /// behind it with a `stop_reason` and usage — unless a tool input it carries
    /// does not parse, which marks the completion here rather than erroring.
    pub(super) fn finish(mut self) -> Result<Completion, Error> {
        let mut upstream_failure: Option<UpstreamFailureRecord> = if self.finalized {
            if let Some(why) = &self.failure {
                log::warn!("stream error after finalization (completion kept): {why}");
            }
            None
        } else {
            Some(
                self.failure
                    .take()
                    .unwrap_or_else(|| {
                        "stream ended before finalizing (no message_stop)".to_string()
                    })
                    .into(),
            )
        };
        let mut output = Vec::new();
        let mut has_calls = false;
        for block in self.blocks {
            match block {
                BlockAccum::Text(text) => output.push(MessagesOutputBlock::Text(text)),
                BlockAccum::Thinking { text: t, signature } => {
                    output.push(MessagesOutputBlock::Thinking { text: t, signature })
                }
                BlockAccum::Redacted { data } => {
                    output.push(MessagesOutputBlock::RedactedThinking { data })
                }
                BlockAccum::ToolUse {
                    id,
                    name,
                    partial_json,
                } => {
                    let input = if partial_json.trim().is_empty() {
                        Value::Object(serde_json::Map::new())
                    } else {
                        match serde_json::from_str(&partial_json) {
                            Ok(input) => input,
                            // A tool input that does not parse is not a dispatchable
                            // call, however the stream ended: an aborted stream cuts one
                            // mid-fragment, and a *finalized* stream can deliver
                            // `message_stop` after fragments that end mid-string. Both
                            // become the same record-only partial, marked here for the
                            // same reason the empty-finalized stream below is: the mark
                            // *is* the retryable signal, so the turn loop and the fork
                            // peek each retry it on the budget they already have. An
                            // error here instead takes the request-failed path, which
                            // retries nothing (docs/transport.md).
                            //
                            // The raw bytes are kept either way: the entry is never
                            // projected or dispatched, and may be the only copy of what
                            // the model produced.
                            Err(e) => {
                                log::warn!("partial tool input kept unparsed: {e}");
                                if upstream_failure.is_none() {
                                    upstream_failure = Some(
                                        format!(
                                            "stream finalized with unparseable tool \
                                             input: {e}"
                                        )
                                        .into(),
                                    );
                                }
                                Value::String(partial_json)
                            }
                        }
                    };
                    has_calls = true;
                    output.push(MessagesOutputBlock::ToolUse(ToolCall::new(name, id, input)));
                }
            }
        }
        // A tool call in a completion that stopped at the output-token cap was cut
        // mid-arguments — the model stops at `tool_use`, never at the cap, after a
        // call it finished. Whether the fragments happen to parse is the
        // upstream's doing, not the model's (observed 2026-09-02: a cap-cut
        // `write` came back as a valid object with its `contents` split across
        // several nonsense keys and no `path`, and was dispatched), so the call
        // is marked undispatchable here on the stop reason alone, the same
        // record-only partial an unparseable input becomes (docs/transport.md).
        if upstream_failure.is_none()
            && has_calls
            && self.stop_reason.as_deref() == Some("max_tokens")
        {
            log::warn!("tool call cut at the output-token cap kept undispatched");
            upstream_failure = Some(
                "stream stopped at the output-token cap mid-tool-call"
                    .to_string()
                    .into(),
            );
        }
        // A stream that finalizes with **nothing** is the complement of one that
        // dies before finalizing (docs/transport.md), and is marked the same way.
        // Marking it here rather than in the loop is what makes both surfaces
        // handle it: the mark *is* the record-only/retryable signal, so the turn
        // loop and the fork peek each retry it on the budget they already have.
        //
        // Zero output tokens is required as corroboration, so a model that
        // legitimately answers with no blocks — but was billed for the thinking
        // that got it there — is not mistaken for this and retried into
        // exhaustion.
        //
        // This is the one path that attaches `StreamDiagnostics`, because it is
        // the one where nothing else survives: the generation id such a stream
        // carries 404s permanently, so the log is the only place a later
        // investigation can read from.
        let upstream_failure = upstream_failure.or_else(|| {
            let empty = output.is_empty();
            (empty && self.usage.output_tokens == 0).then(|| {
                let (status, headers) = match self.response.take() {
                    Some(ResponseMeta { status, headers }) => (status, headers),
                    None => (0, BTreeMap::new()),
                };
                UpstreamFailureRecord {
                    why: "stream finalized with no content and no usage".to_string(),
                    stream: Some(StreamDiagnostics {
                        status,
                        headers,
                        message_start: self.message_start.take(),
                        frames: std::mem::take(&mut self.frames),
                        frames_truncated: self.frames_truncated,
                    }),
                }
            })
        });
        Ok(Completion {
            assistant: Payload::Assistant {
                output: AssistantOutput::Messages(output),
                upstream_failure,
            },
            id: self.id,
            model: self.model,
            stop_reason: self.stop_reason,
            usage: self.usage,
            failure_class: self.failure_class,
        })
    }
}
