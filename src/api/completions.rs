//! The Chat Completions request: semantic turns → `messages[]`, plus the envelope
//! (docs/transport.md). Assistant turns recorded in this dialect replay as
//! received; the `wire --dialect completions` dev view also translates turns
//! recorded in Messages.

use serde::Serialize;
use serde_json::{Map, Value};

use super::MAX_TOKENS;
use super::thinking::ToolChoice;
use crate::log::{AssistantOutput, CompletionsMessage, MessagesOutputBlock, ResultImage};
use crate::project::{UserPart, WireTurn};

pub(crate) mod assembler;
#[cfg(test)]
mod test;

/// The request body. Field order is pinned by
/// `completions_envelope_matches_expected_wire_bytes`.
#[derive(Serialize)]
pub(super) struct CompletionsRequest<'a> {
    model: &'a str,
    /// Spelled `max_completion_tokens` by OpenAI's own endpoint; `max_tokens` is
    /// what the other servers accept (backlog/completions-dialect/dialect.md).
    max_tokens: u32,
    messages: Vec<Message>,
    tools: Vec<Value>,
    tool_choice: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    stream: bool,
    stream_options: StreamOptions,
}

/// Asks for the usage chunk at the end of the stream; without it most servers
/// report no usage on a streamed response.
#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// One `messages[]` entry.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: UserContent,
    },
    /// Every key the message arrived with, under its arrival name; `role` is the
    /// tag, so the stored message does not carry it.
    Assistant(CompletionsMessage),
    /// A tool result. Carries text only: a result's images ride the `user`
    /// message after the run of tool messages ([`encode_user`]). Completions has
    /// no error flag, so `is_error` does not reach this wire, as on Responses.
    Tool {
        tool_call_id: String,
        content: String,
    },
}

/// A user message's content: a bare string for one text part, the parts array
/// otherwise. Some local servers accept only the string form, and the choice is a
/// function of the parts alone, so it stays deterministic.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ImageUrl {
    pub url: String,
}

/// `tool_choice` in the Completions spelling.
fn tool_choice_value(choice: &ToolChoice) -> Value {
    match choice.forced() {
        Some(name) => serde_json::json!({"type": "function", "function": {"name": name}}),
        None => Value::String("auto".to_string()),
    }
}

/// Function tools from the harness's Anthropic-shaped definitions
/// (`{name, description, input_schema}`).
fn tool_definitions(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").cloned().unwrap_or(Value::Null),
                    "description": tool.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": tool.get("input_schema").cloned().unwrap_or(Value::Null),
                },
            })
        })
        .collect()
}

/// Encode semantic turns as Completions `messages`, the preamble leading as a
/// `system` message. Assistant turns recorded in this dialect replay as stored;
/// another dialect's turns are skipped, which `project::check_dialect` makes
/// unreachable for a live request. Breakpoints place nothing: this wire has no
/// per-part cache marker.
pub fn encode_completions(turns: &[WireTurn], preamble: &str) -> Vec<Message> {
    encode(turns, preamble, |output| match output {
        AssistantOutput::Completions(message) => Ok(Some(message.clone())),
        AssistantOutput::Messages(_) | AssistantOutput::Responses(_) => Ok(None),
    })
    .expect("the replaying encoder refuses nothing")
}

/// [`encode_completions`] for the `wire --dialect completions` view, which also
/// translates turns recorded in Messages: thinking becomes OpenRouter's
/// `reasoning_details`, tagged by [`reasoning_format`] of `model`. Refuses a
/// Responses-recorded turn, which it does not translate.
pub fn translate_completions(
    turns: &[WireTurn],
    preamble: &str,
    model: &str,
) -> Result<Vec<Message>, super::Error> {
    let format = reasoning_format(model);
    encode(turns, preamble, |output| match output {
        AssistantOutput::Completions(message) => Ok(Some(message.clone())),
        AssistantOutput::Messages(blocks) => Ok(Some(translate_assistant(blocks, format))),
        AssistantOutput::Responses(_) => Err("the Completions view translates sessions \
                                              recorded in the Messages dialect; this one \
                                              is recorded in Responses"
            .into()),
    })
}

fn encode(
    turns: &[WireTurn],
    preamble: &str,
    assistant: impl Fn(&AssistantOutput) -> Result<Option<CompletionsMessage>, super::Error>,
) -> Result<Vec<Message>, super::Error> {
    let mut messages = Vec::new();
    if !preamble.is_empty() {
        messages.push(Message::System {
            content: preamble.to_string(),
        });
    }
    for turn in turns {
        match turn {
            WireTurn::User(parts) => encode_user(parts, &mut messages),
            WireTurn::Assistant(output) => {
                messages.extend(assistant(output)?.map(Message::Assistant));
            }
            WireTurn::Breakpoint => {}
        }
    }
    Ok(messages)
}

/// Render the full Completions envelope for `turns` as pretty JSON — the
/// `wire --dialect completions` view.
pub fn render_completions_request_pretty(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    turns: &[WireTurn],
) -> Result<String, super::Error> {
    let messages = translate_completions(turns, system, model)?;
    Ok(serde_json::to_string_pretty(&completions_request(
        model,
        messages,
        tools,
        tool_choice,
        MAX_TOKENS,
        None,
    ))?)
}

/// The request body a turn sends (`Client::complete`).
pub(super) fn completions_request<'a>(
    model: &'a str,
    messages: Vec<Message>,
    tools: &[Value],
    tool_choice: ToolChoice,
    max_tokens: u32,
    temperature: Option<f64>,
) -> CompletionsRequest<'a> {
    CompletionsRequest {
        model,
        max_tokens,
        messages,
        tools: tool_definitions(tools),
        tool_choice: tool_choice_value(&tool_choice),
        temperature,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
    }
}

/// A user turn: one `tool` message per result, in order, then one `user` message
/// carrying the prose and every result's images in log order. A `tool` message
/// must follow the assistant's `tool_calls` directly and cannot carry an image, so
/// the images and any prose wait for the run of results to end — a fixed position,
/// so the encoding stays deterministic.
fn encode_user(parts: &[UserPart], messages: &mut Vec<Message>) {
    let mut content = Vec::new();
    for part in parts {
        match part {
            UserPart::Text(text) => content.push(ContentPart::Text { text: text.clone() }),
            UserPart::ToolResult {
                call_id,
                content: text,
                images,
                ..
            } => {
                messages.push(Message::Tool {
                    tool_call_id: call_id.0.clone(),
                    content: text.clone(),
                });
                content.extend(images.iter().map(image_part));
            }
            UserPart::Breakpoint => {}
        }
    }
    let content = match <[ContentPart; 1]>::try_from(content) {
        Ok([ContentPart::Text { text }]) => UserContent::Text(text),
        Ok(one) => UserContent::Parts(one.into()),
        Err(parts) if parts.is_empty() => return,
        Err(parts) => UserContent::Parts(parts),
    };
    messages.push(Message::User { content });
}

fn image_part(image: &ResultImage) -> ContentPart {
    ContentPart::ImageUrl {
        image_url: ImageUrl {
            url: format!("data:{};base64,{}", image.media_type, image.data),
        },
    }
}

/// The `format` tag OpenRouter's `reasoning_details` carries for a signature's
/// origin. A Messages log records no vendor, so the view infers one from the model
/// id: Gemini models through OpenRouter, Anthropic otherwise. View-only — the
/// dialect replays stored keys and never writes this tag.
fn reasoning_format(model: &str) -> &'static str {
    if model.starts_with("google/") || model.starts_with("gemini") {
        "google-gemini-v1"
    } else {
        "anthropic-claude-v1"
    }
}

/// A Messages assistant turn in Completions shape: text blocks joined by newlines
/// into `content` (empty when there are none), `tool_use` blocks as `tool_calls`,
/// and thinking as `reasoning_details` entries indexed in block order.
fn translate_assistant(blocks: &[MessagesOutputBlock], format: &str) -> CompletionsMessage {
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = Vec::new();
    for block in blocks {
        match block {
            MessagesOutputBlock::Text(t) => text.push(t.as_str()),
            MessagesOutputBlock::Thinking {
                text: thinking,
                signature,
            } => reasoning.push(serde_json::json!({
                "type": "reasoning.text",
                "text": thinking,
                "signature": (!signature.is_empty()).then_some(signature),
                "id": null,
                "format": format,
                "index": reasoning.len(),
            })),
            MessagesOutputBlock::RedactedThinking { data } => reasoning.push(serde_json::json!({
                "type": "reasoning.encrypted",
                "data": data,
                "id": null,
                "format": format,
                "index": reasoning.len(),
            })),
            MessagesOutputBlock::ToolUse(call) => tool_calls.push(serde_json::json!({
                "id": call.id.0,
                "type": "function",
                "function": {"name": call.name, "arguments": call.input.to_string()},
            })),
        }
    }
    let mut message = Map::new();
    message.insert("content".into(), Value::String(text.join("\n")));
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    if !reasoning.is_empty() {
        message.insert("reasoning_details".into(), Value::Array(reasoning));
    }
    CompletionsMessage::from_map(message)
}
