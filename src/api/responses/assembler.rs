//! Fold the Responses SSE event stream into one [`Completion`]
//! (docs/transport.md). Mirrors the Messages assembler's contract: deltas feed
//! the live view, the terminal event's `output` is the committed turn, and a
//! stream that never finalizes yields a record-only partial.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::wire::item_from_wire;
use crate::api::assembler::ResponseMeta;
use crate::api::{Completion, Error, Streamed, Usage};
use crate::log::{
    AssistantOutput, FRAME_CAPTURE_CAP, Payload, ResponsesOutputItem, StreamDiagnostics,
    UpstreamFailureRecord,
};

pub(crate) struct ResponsesAssembler {
    /// Items by `output_index`, from `response.output_item.done`. Superseded by
    /// the terminal event's `output` when that arrives, which is the complete
    /// form (an `added` item's `encrypted_content` may be partial).
    items: BTreeMap<usize, ResponsesOutputItem>,
    id: Option<String>,
    model: String,
    /// The terminal `response.status`, once seen.
    status: Option<String>,
    /// `response.incomplete_details.reason`, once seen.
    incomplete_reason: Option<String>,
    usage: Usage,
    finalized: bool,
    failure: Option<String>,
    response: Option<ResponseMeta>,
    frames: String,
    frames_truncated: bool,
    created: Option<String>,
}

/// The `usage` object on a terminal event. `input_tokens` is the whole prompt,
/// cached and written tokens included — the opposite of Messages, whose
/// `input_tokens` excludes both. [`Self::normalized`] restates it in the
/// harness's three-way split so every consumer's sum stays the prompt total.
#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: InputDetails,
}

#[derive(Deserialize, Default)]
struct InputDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

impl WireUsage {
    fn normalized(&self) -> Usage {
        let cached = self.input_tokens_details.cached_tokens;
        let written = self.input_tokens_details.cache_write_tokens;
        Usage {
            input_tokens: self
                .input_tokens
                .saturating_sub(cached)
                .saturating_sub(written),
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: written,
            cache_read_input_tokens: cached,
        }
    }
}

#[derive(Deserialize)]
struct ResponseBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Option<Vec<Value>>,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct IncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl ResponsesAssembler {
    pub(crate) fn new() -> Self {
        Self {
            items: BTreeMap::new(),
            id: None,
            model: String::new(),
            status: None,
            incomplete_reason: None,
            usage: Usage::default(),
            finalized: false,
            failure: None,
            response: None,
            frames: String::new(),
            frames_truncated: false,
            created: None,
        }
    }

    pub(crate) fn observed_response(&mut self, response: ResponseMeta) {
        self.response = Some(response);
    }

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

    pub(crate) fn fail(&mut self, why: String) {
        self.failure = Some(why);
    }

    fn absorb_response(&mut self, body: &Value) {
        let Ok(response) = serde_json::from_value::<ResponseBody>(body.clone()) else {
            return;
        };
        if let Some(id) = response.id {
            self.id = Some(id);
        }
        if let Some(model) = response.model {
            self.model = model;
        }
        if let Some(status) = response.status {
            self.status = Some(status);
        }
        if let Some(usage) = response.usage {
            self.usage.merge_max(&usage.normalized());
        }
        if let Some(details) = response.incomplete_details {
            self.incomplete_reason = details.reason;
        }
        if let Some(output) = response.output
            && !output.is_empty()
        {
            self.items = output
                .into_iter()
                .enumerate()
                .map(|(i, item)| (i, item_from_wire(item)))
                .collect();
        }
        if let Some(error) = response.error
            && self.failure.is_none()
        {
            self.failure = Some(format!(
                "upstream response failed: {} ({})",
                error.message.unwrap_or_default(),
                error.code.unwrap_or_default()
            ));
        }
    }

    /// Fold one SSE event. The event's `type` is read off the JSON body — the
    /// `event:` line carries the same string but the body is what the schema
    /// defines. Events outside the handled set are ignored under one rule: the
    /// terminal event's `output` carries every item whole, so no semantic output
    /// depends on an intermediate event being understood.
    pub(crate) fn apply(
        &mut self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<Option<Streamed>, Error> {
        self.capture_frame(event_type, data);
        let body: Value = serde_json::from_str(data)
            .map_err(|e| -> Error { format!("unparseable Responses event: {e}").into() })?;
        let kind = body
            .get("type")
            .and_then(Value::as_str)
            .or(event_type)
            .unwrap_or("");
        let index = body
            .get("output_index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .unwrap_or(0);
        let delta = || {
            body.get("delta")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default()
        };
        let streamed = match kind {
            "response.created" => {
                if self.created.is_none() {
                    self.created = Some(data.to_string());
                }
                if let Some(response) = body.get("response") {
                    self.absorb_response(response);
                }
                None
            }
            "response.output_item.added" => {
                let item = body.get("item");
                let is_call = item.and_then(|i| i.get("type")).and_then(Value::as_str)
                    == Some("function_call");
                if is_call {
                    let id = item
                        .and_then(|i| i.get("call_id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .and_then(|i| i.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(Streamed::ToolStart { index, id, name })
                } else {
                    None
                }
            }
            "response.output_item.done" => {
                if let Some(item) = body.get("item") {
                    self.items.insert(index, item_from_wire(item.clone()));
                }
                None
            }
            "response.output_text.delta" => Some(Streamed::Text {
                index,
                text: delta(),
            }),
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                Some(Streamed::Thinking {
                    index,
                    text: delta(),
                })
            }
            "response.function_call_arguments.delta" => Some(Streamed::ToolInput {
                index,
                fragment: delta(),
            }),
            "response.completed" | "response.incomplete" => {
                if let Some(response) = body.get("response") {
                    self.absorb_response(response);
                }
                self.finalized = true;
                None
            }
            "response.failed" => {
                if let Some(response) = body.get("response") {
                    self.absorb_response(response);
                }
                if self.failure.is_none() {
                    self.failure = Some("upstream response failed".to_string());
                }
                None
            }
            "error" => {
                let message = body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)");
                return Err(format!("upstream stream error: {message} — {data}").into());
            }
            _ => None,
        };
        Ok(streamed)
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
                        "stream ended before finalizing (no response.completed)".to_string()
                    })
                    .into(),
            )
        };
        let items: Vec<ResponsesOutputItem> =
            std::mem::take(&mut self.items).into_values().collect();
        let mut has_calls = false;
        for item in &items {
            match item {
                ResponsesOutputItem::FunctionCall {
                    arguments, name, ..
                } => {
                    has_calls = true;
                    if let Err(e) = serde_json::from_str::<Value>(arguments)
                        && upstream_failure.is_none()
                    {
                        upstream_failure = Some(
                            format!("function call `{name}` has unparseable arguments: {e}").into(),
                        );
                    }
                }
                ResponsesOutputItem::Unsupported(value) => {
                    if upstream_failure.is_none() {
                        let kind = value.get("type").and_then(Value::as_str).unwrap_or("?");
                        upstream_failure = Some(
                            format!("response carried an unsupported output item `{kind}`").into(),
                        );
                    }
                }
                ResponsesOutputItem::Reasoning { .. } | ResponsesOutputItem::Message { .. } => {}
            }
        }
        // The normalized stop reason `agent.rs` reads: the Messages spellings,
        // so the loop compares one vocabulary (docs/transport.md).
        let stop_reason = match (self.status.as_deref(), self.incomplete_reason.as_deref()) {
            (Some("completed"), _) if has_calls => Some("tool_use".to_string()),
            (Some("completed"), _) => Some("end_turn".to_string()),
            (Some("incomplete"), Some("max_output_tokens")) => Some("max_tokens".to_string()),
            (Some("incomplete"), Some(reason)) => Some(reason.to_string()),
            (Some(other), _) => Some(other.to_string()),
            (None, _) => None,
        };
        if upstream_failure.is_none() && has_calls && stop_reason.as_deref() == Some("max_tokens") {
            log::warn!("tool call cut at the output-token cap kept undispatched");
            upstream_failure = Some(
                "stream stopped at the output-token cap mid-tool-call"
                    .to_string()
                    .into(),
            );
        }
        let upstream_failure = upstream_failure.or_else(|| {
            (items.is_empty() && self.usage.output_tokens == 0).then(|| {
                let (status, headers) = match self.response.take() {
                    Some(ResponseMeta { status, headers }) => (status, headers),
                    None => (0, BTreeMap::new()),
                };
                UpstreamFailureRecord {
                    why: "stream finalized with no content and no usage".to_string(),
                    stream: Some(StreamDiagnostics {
                        status,
                        headers,
                        message_start: self.created.take(),
                        frames: std::mem::take(&mut self.frames),
                        frames_truncated: self.frames_truncated,
                    }),
                }
            })
        });
        Ok(Completion {
            assistant: Payload::Assistant {
                output: AssistantOutput::Responses(items),
                upstream_failure,
            },
            id: self.id,
            model: self.model,
            stop_reason,
            usage: self.usage,
        })
    }
}
