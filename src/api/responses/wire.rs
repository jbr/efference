//! The OpenAI Responses wire shapes efference reads and writes (docs/transport.md).
//!
//! Output items arrive on the stream and are stored in the log's own vocabulary
//! ([`ResponsesOutputItem`]); the same items go back out as `input` on the next
//! request. The two conversions here are the only place the wire spelling and
//! the log spelling meet, so a replay is a copy of what the provider returned.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log::{
    CallId, ResponsesMessageContent, ResponsesOutputItem, ResponsesPhase,
    ResponsesReasoningSummary, ResponsesReasoningText,
};

/// One `input` item on the request.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Message {
        role: InputRole,
        content: Vec<InputContent>,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionOutput,
    },
    Reasoning(WireReasoning),
    /// An assistant message replayed as the provider emitted it.
    #[serde(rename = "message")]
    AssistantMessage(WireMessage),
    FunctionCall(WireFunctionCall),
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InputRole {
    Developer,
    User,
}

/// A content part of a user or developer message, or of a function result.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContent {
    InputText {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_breakpoint: Option<CacheBreakpoint>,
    },
    InputImage {
        /// A `data:` URL; the log stores media type and base64 separately.
        image_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_breakpoint: Option<CacheBreakpoint>,
    },
}

impl InputContent {
    pub fn breakpoint_mut(&mut self) -> &mut Option<CacheBreakpoint> {
        match self {
            Self::InputText {
                prompt_cache_breakpoint,
                ..
            }
            | Self::InputImage {
                prompt_cache_breakpoint,
                ..
            } => prompt_cache_breakpoint,
        }
    }
}

/// `prompt_cache_breakpoint: {"mode":"explicit"}` — the explicit cache-write
/// marker on GPT-5.6 and later.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheBreakpoint {
    pub mode: &'static str,
}

impl CacheBreakpoint {
    pub const EXPLICIT: Self = Self { mode: "explicit" };
}

/// A function result's `output`: a bare string, or parts when images ride along.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum FunctionOutput {
    Text(String),
    Parts(Vec<InputContent>),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WireReasoning {
    pub id: String,
    #[serde(default)]
    pub summary: Vec<WireSummaryPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<WireReasoningText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireSummaryPart {
    SummaryText { text: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireReasoningText {
    ReasoningText { text: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WireMessage {
    pub id: String,
    pub role: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ResponsesPhase>,
    #[serde(default)]
    pub content: Vec<WireMessageContent>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireMessageContent {
    OutputText {
        text: String,
        #[serde(default)]
        annotations: Vec<Value>,
    },
    Refusal {
        refusal: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WireFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Lift one output item off the wire into the log vocabulary. Anything but the
/// three forms the first integration handles is kept whole as `Unsupported`, which
/// the assembler treats as record-only (docs/transport.md).
pub fn item_from_wire(value: Value) -> ResponsesOutputItem {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let lifted = match kind {
        "reasoning" => serde_json::from_value::<WireReasoning>(value.clone())
            .ok()
            .map(|r| ResponsesOutputItem::Reasoning {
                id: r.id,
                status: r.status,
                summary: r
                    .summary
                    .into_iter()
                    .map(|WireSummaryPart::SummaryText { text }| ResponsesReasoningSummary { text })
                    .collect(),
                content: r
                    .content
                    .into_iter()
                    .map(
                        |WireReasoningText::ReasoningText { text }| ResponsesReasoningText { text },
                    )
                    .collect(),
                encrypted_content: r.encrypted_content,
            }),
        "message" => serde_json::from_value::<WireMessage>(value.clone())
            .ok()
            .filter(|m| m.role == "assistant")
            .map(|m| ResponsesOutputItem::Message {
                id: m.id,
                status: m.status,
                phase: m.phase,
                content: m
                    .content
                    .into_iter()
                    .map(|part| match part {
                        WireMessageContent::OutputText { text, annotations } => {
                            ResponsesMessageContent::OutputText { text, annotations }
                        }
                        WireMessageContent::Refusal { refusal } => {
                            ResponsesMessageContent::Refusal { refusal }
                        }
                    })
                    .collect(),
            }),
        "function_call" => serde_json::from_value::<WireFunctionCall>(value.clone())
            .ok()
            .map(|f| ResponsesOutputItem::FunctionCall {
                id: f.id,
                call_id: CallId(f.call_id),
                name: f.name,
                arguments: f.arguments,
                status: f.status,
            }),
        _ => None,
    };
    lifted.unwrap_or(ResponsesOutputItem::Unsupported(value))
}

/// The replay form of one logged output item. `None` for an unsupported item —
/// a turn holding one is record-only and never projected, so this is unreachable
/// from the encoder in practice.
pub fn item_to_wire(item: &ResponsesOutputItem) -> Option<InputItem> {
    Some(match item {
        ResponsesOutputItem::Reasoning {
            id,
            status,
            summary,
            content,
            encrypted_content,
        } => InputItem::Reasoning(WireReasoning {
            id: id.clone(),
            summary: summary
                .iter()
                .map(|s| WireSummaryPart::SummaryText {
                    text: s.text.clone(),
                })
                .collect(),
            content: content
                .iter()
                .map(|c| WireReasoningText::ReasoningText {
                    text: c.text.clone(),
                })
                .collect(),
            encrypted_content: encrypted_content.clone(),
            status: status.clone(),
        }),
        ResponsesOutputItem::Message {
            id,
            status,
            phase,
            content,
        } => InputItem::AssistantMessage(WireMessage {
            id: id.clone(),
            role: "assistant".to_string(),
            status: status.clone(),
            phase: *phase,
            content: content
                .iter()
                .map(|part| match part {
                    ResponsesMessageContent::OutputText { text, annotations } => {
                        WireMessageContent::OutputText {
                            text: text.clone(),
                            annotations: annotations.clone(),
                        }
                    }
                    ResponsesMessageContent::Refusal { refusal } => WireMessageContent::Refusal {
                        refusal: refusal.clone(),
                    },
                })
                .collect(),
        }),
        ResponsesOutputItem::FunctionCall {
            id,
            call_id,
            name,
            arguments,
            status,
        } => InputItem::FunctionCall(WireFunctionCall {
            id: id.clone(),
            call_id: call_id.0.clone(),
            name: name.clone(),
            arguments: arguments.clone(),
            status: status.clone(),
        }),
        ResponsesOutputItem::Unsupported(_) => return None,
    })
}
