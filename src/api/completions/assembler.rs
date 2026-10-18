//! Fold a Chat Completions SSE stream into one [`Completion`] (docs/transport.md).
//! Each chunk's `choices[0].delta` merges into the assistant message under one
//! recursive rule ([`merge`]), so every vendor's reasoning keys assemble without
//! the assembler knowing them; deltas also feed the live view. Mirrors the other
//! assemblers' contract: a stream that never finishes yields a record-only partial.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::api::assembler::ResponseMeta;
use crate::api::failure::{FailureClass, classify};
use crate::api::{Completion, Error, Finish, FinishReason, Receipt, Streamed, Usage, UsageCost};
use crate::log::{
    AssistantOutput, CompletionsMessage, Exclusion, FRAME_CAPTURE_CAP, Payload, StreamDiagnostics,
    UpstreamFailureRecord,
};

#[cfg(test)]
mod test;

/// Keys a server repeats whole in every fragment rather than splitting: the last
/// value wins instead of concatenating. OpenRouter repeats `role` in every delta
/// and `type` / `format` in every `reasoning_details` fragment (measured,
/// backlog/inbox/completions-dialect.md).
const IDENTITY: [&str; 5] = ["id", "type", "role", "format", "index"];

/// Live-view indices. Completions has no block index of its own, so each kind of
/// output takes a fixed slot and a tool call `i` takes `TOOL_BASE + i`, which keeps
/// a client's per-index buffers apart.
const THINKING_INDEX: usize = 0;
const TEXT_INDEX: usize = 1;
const TOOL_BASE: usize = 2;

pub(crate) struct CompletionsAssembler {
    /// The assistant message so far: every delta merged in.
    message: Value,
    id: Option<String>,
    model: String,
    finish_reason: Option<String>,
    /// `[DONE]` or a `finish_reason` arrived: the message is whole.
    finalized: bool,
    usage: Usage,
    receipt: Receipt,
    /// Tool-call indices whose [`Streamed::ToolStart`] has been sent.
    started: BTreeSet<u64>,
    failure: Option<String>,
    failure_class: FailureClass,
    response: Option<ResponseMeta>,
    first_chunk: Option<String>,
    frames: String,
    frames_truncated: bool,
}

/// The `usage` object. `prompt_tokens` is the whole prompt, cached tokens
/// included, as on Responses; [`Self::normalized`] restates it in the harness's
/// split so every consumer's sum stays the prompt total.
#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Deserialize, Default)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

impl WireUsage {
    fn normalized(&self) -> Usage {
        let cached = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .unwrap_or(0);
        Usage {
            input_tokens: self.prompt_tokens.unwrap_or(0).saturating_sub(cached),
            output_tokens: self.completion_tokens.unwrap_or(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: cached,
        }
    }
}

impl CompletionsAssembler {
    pub(crate) fn new() -> Self {
        Self {
            message: Value::Object(Map::new()),
            id: None,
            model: String::new(),
            finish_reason: None,
            finalized: false,
            usage: Usage::default(),
            receipt: Receipt::default(),
            started: BTreeSet::new(),
            failure: None,
            failure_class: FailureClass::Transient,
            response: None,
            first_chunk: None,
            frames: String::new(),
            frames_truncated: false,
        }
    }

    pub(crate) fn observed_response(&mut self, response: ResponseMeta) {
        self.response = Some(response);
    }

    pub(crate) fn fail(&mut self, why: String) {
        self.failure = Some(why);
    }

    fn capture_frame(&mut self, data: &str) {
        if self.frames_truncated {
            return;
        }
        let frame = format!("data: {data}\n\n");
        if self.frames.len() + frame.len() > FRAME_CAPTURE_CAP {
            self.frames_truncated = true;
            return;
        }
        self.frames.push_str(&frame);
    }

    /// Fold one SSE event. Chunks carry no event type; `[DONE]` ends the stream.
    /// Keys outside `choices[0].delta` and the handful read here (the padding
    /// Mistral sends as `p`, Inception's `server_timing`) are ignored.
    pub(crate) fn apply(
        &mut self,
        _event_type: Option<&str>,
        data: &str,
    ) -> Result<Option<Streamed>, Error> {
        self.capture_frame(data);
        if data.trim() == "[DONE]" {
            self.finalized = true;
            return Ok(None);
        }
        let chunk: Value = serde_json::from_str(data).map_err(|e| -> Error {
            let head: String = data.chars().take(120).collect();
            format!("unparseable Completions chunk: {e}: {head:?}").into()
        })?;
        if self.first_chunk.is_none() {
            self.first_chunk = Some(data.to_string());
        }
        if let Some(error) = chunk.get("error") {
            let field = |name: &str| error.get(name).and_then(Value::as_str);
            let code = error
                .get("code")
                .map(|code| match code {
                    Value::String(code) => code.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            self.failure_class = classify(field("type"), Some(&code));
            let message = field("message").unwrap_or("(no message)");
            return Err(format!("upstream stream error [{code}]: {message} — {data}").into());
        }
        if self.id.is_none() {
            self.id = chunk.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            self.model = model.to_string();
        }
        if let Some(provider) = chunk.get("provider").and_then(Value::as_str) {
            self.receipt.served_by = Some(provider.to_string());
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            if let Ok(wire) = WireUsage::deserialize(usage) {
                self.usage.merge_max(&wire.normalized());
            }
            if let Some(cost) = UsageCost::deserialize(usage)
                .ok()
                .and_then(UsageCost::into_inline)
            {
                self.receipt.cost = Some(cost);
            }
        }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
            return Ok(None);
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_string());
            self.finalized = true;
        }
        let Some(delta) = choice.get("delta").filter(|delta| delta.is_object()) else {
            return Ok(None);
        };
        let streamed = self.live(delta);
        let message = std::mem::take(&mut self.message);
        self.message = merge(message, delta.clone(), None);
        Ok(streamed)
    }

    /// What one delta shows the live view. A chunk can carry several kinds at
    /// once (OpenRouter's reasoning beside an empty `content`); the first
    /// non-empty one in the order call, reasoning, prose is reported. The
    /// committed message is the merge, so nothing depends on this choice.
    fn live(&mut self, delta: &Value) -> Option<Streamed> {
        if let Some(Value::Array(calls)) = delta.get("tool_calls") {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let slot = TOOL_BASE + index as usize;
                let function = call.get("function");
                if self.started.insert(index) {
                    let field = |value: Option<&Value>| {
                        value
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    return Some(Streamed::ToolStart {
                        index: slot,
                        id: field(call.get("id")),
                        name: field(function.and_then(|f| f.get("name"))),
                    });
                }
                let fragment = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !fragment.is_empty() {
                    return Some(Streamed::ToolInput {
                        index: slot,
                        fragment: fragment.to_string(),
                    });
                }
            }
        }
        let Value::Object(map) = delta else {
            return None;
        };
        let reasoning = CompletionsMessage::from_map(map.clone())
            .reasoning_text()
            .concat();
        if !reasoning.is_empty() {
            return Some(Streamed::Thinking {
                index: THINKING_INDEX,
                text: reasoning,
            });
        }
        let text = CompletionsMessage::from_map(map.clone()).text().concat();
        (!text.is_empty()).then_some(Streamed::Text {
            index: TEXT_INDEX,
            text,
        })
    }

    pub(crate) fn finish(mut self) -> Result<Completion, Error> {
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
                        "stream ended before finishing (no finish_reason or [DONE])".to_string()
                    })
                    .into(),
            )
        };
        let Value::Object(map) = std::mem::take(&mut self.message) else {
            unreachable!("the message starts as an object and merges as one")
        };
        let message = CompletionsMessage::from_assembled(map);
        let has_calls = match message.tool_calls() {
            Ok(calls) => !calls.is_empty(),
            Err(e) => {
                if upstream_failure.is_none() {
                    upstream_failure =
                        Some(format!("a tool call has unparseable arguments: {e}").into());
                }
                true
            }
        };
        let finish = self
            .finish_reason
            .as_deref()
            .map(|reason| Finish::completions(reason, has_calls));
        let capped = finish
            .as_ref()
            .is_some_and(|finish| finish.reason == FinishReason::OutputCap);
        if upstream_failure.is_none() && has_calls && capped {
            log::warn!("tool call cut at the output-token cap kept undispatched");
            upstream_failure = Some(
                "stream stopped at the output-token cap mid-tool-call"
                    .to_string()
                    .into(),
            );
        }
        let upstream_failure = upstream_failure.or_else(|| {
            (message.is_empty() && self.usage.output_tokens == 0).then(|| {
                let (status, headers) = match self.response.take() {
                    Some(ResponseMeta { status, headers }) => (status, headers),
                    None => (0, BTreeMap::new()),
                };
                UpstreamFailureRecord {
                    why: "stream finished with no content and no usage".to_string(),
                    stream: Some(StreamDiagnostics {
                        status,
                        headers,
                        message_start: self.first_chunk.take(),
                        frames: std::mem::take(&mut self.frames),
                        frames_truncated: self.frames_truncated,
                    }),
                }
            })
        });
        Ok(Completion {
            assistant: Payload::Assistant {
                output: AssistantOutput::Completions(message),
                excluded: upstream_failure.map(Exclusion::Upstream),
            },
            id: self.id,
            model: self.model,
            finish,
            usage: self.usage,
            cache_miss: None,
            receipt: self.receipt,
            failure_class: self.failure_class,
        })
    }
}

/// Merge one delta into what has arrived (backlog/inbox/completions-dialect.md,
/// where each clause's measured cause is recorded):
///
/// - `null` is absent and never overwrites;
/// - strings concatenate, except under [`IDENTITY`] keys, where the last value wins;
/// - objects merge key by key;
/// - an array element with `index` merges into the element with the same `index`
///   and a matching `type` (a fragment without `type` matches: OpenRouter's later
///   argument fragments), else appends. Gemini's thought signature arrives as a
///   `reasoning.encrypted` detail at the same `index` as its `reasoning.text`;
/// - an element without `index` continues the last element when that one has none
///   either and the same `type` (Mistral's `thinking` parts), else appends;
/// - `content` is a string or a parts array, a string being one `text` part: when
///   the two meet, the string becomes a part and an empty one is dropped;
/// - any other value: the last wins.
fn merge(acc: Value, delta: Value, key: Option<&str>) -> Value {
    match (acc, delta) {
        (acc, Value::Null) => acc,
        (Value::Null, delta) => delta,
        (_, delta) if key.is_some_and(|key| IDENTITY.contains(&key)) => delta,
        (Value::String(mut acc), Value::String(delta)) => {
            acc.push_str(&delta);
            Value::String(acc)
        }
        (Value::Object(mut acc), Value::Object(delta)) => {
            for (k, v) in delta {
                let prior = acc.remove(&k).unwrap_or(Value::Null);
                let merged = merge(prior, v, Some(&k));
                acc.insert(k, merged);
            }
            Value::Object(acc)
        }
        (Value::Array(acc), Value::Array(delta)) => Value::Array(merge_elements(acc, delta)),
        (
            acc @ (Value::String(_) | Value::Array(_)),
            delta @ (Value::String(_) | Value::Array(_)),
        ) if key == Some("content") => {
            let acc = as_parts(acc)
                .into_iter()
                .filter(|part| !is_empty_text(part))
                .collect();
            Value::Array(merge_elements(acc, as_parts(delta)))
        }
        (_, delta) => delta,
    }
}

fn merge_elements(mut acc: Vec<Value>, delta: Vec<Value>) -> Vec<Value> {
    for item in delta {
        let hit = match item.get("index") {
            Some(index) => acc.iter().position(|prior| {
                prior.get("index") == Some(index)
                    && match item.get("type") {
                        None => true,
                        kind => prior.get("type") == kind,
                    }
            }),
            None if item.is_object() => acc
                .last()
                .filter(|last| {
                    last.is_object()
                        && last.get("index").is_none()
                        && last.get("type") == item.get("type")
                })
                .map(|_| acc.len() - 1),
            None => None,
        };
        match hit {
            Some(i) => {
                let prior = std::mem::take(&mut acc[i]);
                acc[i] = merge(prior, item, None);
            }
            None => acc.push(item),
        }
    }
    acc
}

fn as_parts(content: Value) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts,
        Value::String(text) => vec![serde_json::json!({"type": "text", "text": text})],
        other => vec![other],
    }
}

fn is_empty_text(part: &Value) -> bool {
    part.get("type").and_then(Value::as_str) == Some("text")
        && part.get("text").and_then(Value::as_str) == Some("")
}
