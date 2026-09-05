//! The OpenAI Responses request: semantic turns → `input[]`, plus the envelope
//! (docs/transport.md). The stream side is `responses/assembler.rs`; the wire
//! item shapes are `responses/wire.rs`.

use serde::Serialize;
use serde_json::Value;

use super::provider::PromptCachePolicy;
use super::thinking::ToolChoice;
use crate::log::{AssistantOutput, ResultImage};
use crate::project::{UserPart, WireTurn};

pub(crate) mod assembler;
#[cfg(test)]
mod test;
pub(crate) mod wire;

use wire::{CacheBreakpoint, FunctionOutput, InputContent, InputItem, InputRole, item_to_wire};

/// The request body. Field order is pinned by
/// `responses_envelope_matches_expected_wire_bytes`; the provider matches the
/// whole body for caching, so a reorder here moves the production cache.
#[derive(Serialize)]
pub(super) struct ResponsesRequest<'a> {
    pub model: &'a str,
    pub input: Vec<InputItem>,
    /// Function tools in the Responses spelling ([`tool_definitions`]).
    pub tools: Vec<Value>,
    pub tool_choice: Value,
    /// Mirrors Messages `disable_parallel_tool_use: true` — one call per turn
    /// (docs/README.md).
    pub parallel_tool_calls: bool,
    /// Stateless replay: the log is the only history, and encrypted reasoning
    /// arrives by default when nothing is stored (docs/transport.md).
    pub store: bool,
    /// Never `auto`: a history change is the harness's decision, not the
    /// provider's silent drop of leading items.
    pub truncation: &'static str,
    pub max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_options: Option<PromptCacheOptions>,
    pub stream: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub(super) struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// `auto` asks for the visible summary; without it reasoning items carry
    /// only their opaque continuation.
    pub summary: &'static str,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromptCacheOptions {
    pub mode: &'static str,
}

/// `tool_choice` in the Responses spelling.
pub(super) fn tool_choice_value(choice: &ToolChoice) -> Value {
    match choice.forced() {
        Some(name) => serde_json::json!({"type": "function", "name": name}),
        None => Value::String("auto".to_string()),
    }
}

/// Function tools from the harness's Anthropic-shaped definitions
/// (`{name, description, input_schema}`). `strict: false` keeps optional
/// properties optional; strict-schema conversion is a separate decision
/// (backlog: openai-provider/projection.md).
pub(super) fn tool_definitions(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.get("name").cloned().unwrap_or(Value::Null),
                "description": tool.get("description").cloned().unwrap_or(Value::Null),
                "parameters": tool.get("input_schema").cloned().unwrap_or(Value::Null),
                "strict": false,
            })
        })
        .collect()
}

/// Encode semantic turns as Responses `input`. The preamble leads as a developer
/// message (a breakpoint-eligible carrier, which top-level `instructions` is not).
/// A [`WireTurn::Breakpoint`] marks the nearest preceding `input_text` / `input_image`
/// part when `cache` is explicit; other policies place no marker.
pub fn encode_responses(
    turns: &[WireTurn],
    preamble: &str,
    cache: PromptCachePolicy,
) -> Vec<InputItem> {
    let explicit = cache == PromptCachePolicy::Explicit;
    let mut input = Vec::new();
    if !preamble.is_empty() {
        input.push(InputItem::Message {
            role: InputRole::Developer,
            content: vec![InputContent::InputText {
                text: preamble.to_string(),
                prompt_cache_breakpoint: None,
            }],
        });
    }
    for turn in turns {
        match turn {
            WireTurn::User(parts) => encode_user(parts, explicit, &mut input),
            WireTurn::Assistant(AssistantOutput::Responses(items)) => {
                input.extend(items.iter().filter_map(item_to_wire));
            }
            // Messages output belongs to the other dialect (docs/transport.md).
            // Unreachable while provider routing pins one dialect per session.
            WireTurn::Assistant(AssistantOutput::Messages(_)) => {}
            WireTurn::Breakpoint => {
                if explicit {
                    mark_trailing(&mut input);
                }
            }
        }
    }
    input
}

fn encode_user(parts: &[UserPart], explicit: bool, input: &mut Vec<InputItem>) {
    let mut prose: Vec<InputContent> = Vec::new();
    let flush = |prose: &mut Vec<InputContent>, input: &mut Vec<InputItem>| {
        if !prose.is_empty() {
            input.push(InputItem::Message {
                role: InputRole::User,
                content: std::mem::take(prose),
            });
        }
    };
    for part in parts {
        match part {
            UserPart::Text(text) => prose.push(InputContent::InputText {
                text: text.clone(),
                prompt_cache_breakpoint: None,
            }),
            UserPart::ToolResult {
                call_id,
                content,
                images,
                ..
            } => {
                flush(&mut prose, input);
                let output = if images.is_empty() {
                    FunctionOutput::Text(content.clone())
                } else {
                    let mut parts = vec![InputContent::InputText {
                        text: content.clone(),
                        prompt_cache_breakpoint: None,
                    }];
                    parts.extend(images.iter().map(image_part));
                    FunctionOutput::Parts(parts)
                };
                input.push(InputItem::FunctionCallOutput {
                    call_id: call_id.0.clone(),
                    output,
                });
            }
            UserPart::Breakpoint => {
                if explicit {
                    if let Some(last) = prose.last_mut() {
                        *last.breakpoint_mut() = Some(CacheBreakpoint::EXPLICIT);
                    } else {
                        mark_trailing(input);
                    }
                }
            }
        }
    }
    flush(&mut prose, input);
}

fn image_part(image: &ResultImage) -> InputContent {
    InputContent::InputImage {
        image_url: format!("data:{};base64,{}", image.media_type, image.data),
        prompt_cache_breakpoint: None,
    }
}

/// Attach the explicit marker to the last eligible part already encoded: a
/// message part, or a function result (a bare-string result is lifted to its
/// part form to carry it). Assistant items are not carriers, so the marker walks
/// back past them.
fn mark_trailing(input: &mut [InputItem]) {
    for item in input.iter_mut().rev() {
        match item {
            InputItem::Message { content, .. } => {
                if let Some(last) = content.last_mut() {
                    *last.breakpoint_mut() = Some(CacheBreakpoint::EXPLICIT);
                    return;
                }
            }
            InputItem::FunctionCallOutput { output, .. } => {
                if let FunctionOutput::Text(text) = output {
                    *output = FunctionOutput::Parts(vec![InputContent::InputText {
                        text: std::mem::take(text),
                        prompt_cache_breakpoint: None,
                    }]);
                }
                if let FunctionOutput::Parts(parts) = output
                    && let Some(last) = parts.last_mut()
                {
                    *last.breakpoint_mut() = Some(CacheBreakpoint::EXPLICIT);
                    return;
                }
            }
            InputItem::Reasoning(_)
            | InputItem::AssistantMessage(_)
            | InputItem::FunctionCall(_) => {}
        }
    }
}
