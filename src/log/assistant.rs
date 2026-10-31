//! The assistant's output in each provider dialect's own vocabulary (docs/log-and-projection.md).

use super::*;

/// The ordered assistant-output vocabulary of the session's wire dialect.
///
/// The dialect is outside the item vector so one turn cannot mix replay credentials
/// from different providers. Presentation and execution consume semantic accessors;
/// request projection consumes the concrete arm (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AssistantOutput {
    /// The content blocks of one Anthropic-Messages assistant message, in provider
    /// order. OpenRouter's Anthropic-compatible endpoint uses the same shape.
    Messages(Vec<MessagesOutputBlock>),
    /// The top-level `output` items of one OpenAI Responses result, in provider order.
    Responses(Vec<ResponsesOutputItem>),
    /// One Chat Completions assistant message, as the stream assembled it.
    Completions(CompletionsMessage),
}

impl AssistantOutput {
    /// Build a Messages turn from grouped thinking / text / calls, in that order.
    /// A fixture convenience (also used by binary-crate tests, hence `pub`): the
    /// persisted log has no grouped form to migrate, and assemblers append ordered
    /// blocks directly.
    pub fn from_legacy_messages(
        thinking: Vec<ContentBlock>,
        text: Vec<ContentBlock>,
        calls: Vec<ToolCall>,
    ) -> Self {
        let blocks = thinking
            .into_iter()
            .chain(text)
            .map(|block| match block {
                ContentBlock::Text(text) => MessagesOutputBlock::Text(text),
                ContentBlock::Thinking { text, signature } => {
                    MessagesOutputBlock::Thinking { text, signature }
                }
                ContentBlock::RedactedThinking { data } => {
                    MessagesOutputBlock::RedactedThinking { data }
                }
                ContentBlock::Attachment(arrival) => {
                    MessagesOutputBlock::Text(attachment::turn_line(&arrival))
                }
            })
            .chain(calls.into_iter().map(MessagesOutputBlock::ToolUse))
            .collect();
        Self::Messages(blocks)
    }

    /// A turn with no output, in the given dialect — the record-only entry for a
    /// request that never produced a stream (docs/transport.md).
    pub fn empty(dialect: WireDialect) -> Self {
        match dialect {
            WireDialect::Messages => Self::Messages(Vec::new()),
            WireDialect::Responses => Self::Responses(Vec::new()),
            WireDialect::Completions => Self::Completions(Default::default()),
            // A System One request produces no turn, so no record of one exists.
            WireDialect::SystemOne => {
                unreachable!("a System One connection serves no turns")
            }
        }
    }

    /// The wire dialect these items were recorded in (docs/transport.md).
    pub fn dialect(&self) -> WireDialect {
        match self {
            Self::Messages(_) => WireDialect::Messages,
            Self::Responses(_) => WireDialect::Responses,
            Self::Completions(_) => WireDialect::Completions,
        }
    }

    pub fn messages(&self) -> Option<&[MessagesOutputBlock]> {
        match self {
            Self::Messages(blocks) => Some(blocks),
            Self::Responses(_) | Self::Completions(_) => None,
        }
    }

    /// Visible assistant prose in provider order. Refusals are user-visible output
    /// and therefore participate in the same presentation view.
    pub fn text(&self) -> Vec<&str> {
        match self {
            Self::Messages(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    MessagesOutputBlock::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
            Self::Responses(items) => items
                .iter()
                .flat_map(|item| match item {
                    ResponsesOutputItem::Message { content, .. } => content.as_slice(),
                    _ => &[],
                })
                .map(|content| match content {
                    ResponsesMessageContent::OutputText { text, .. } => text.as_str(),
                    ResponsesMessageContent::Refusal { refusal } => refusal.as_str(),
                })
                .collect(),
            Self::Completions(message) => message.text(),
        }
    }

    /// Human-readable reasoning only. Provider replay credentials are deliberately
    /// absent from this view.
    pub fn reasoning_text(&self) -> Vec<&str> {
        match self {
            Self::Messages(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    MessagesOutputBlock::Thinking { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
            Self::Responses(items) => items
                .iter()
                .flat_map(|item| match item {
                    ResponsesOutputItem::Reasoning { summary, .. } => summary.as_slice(),
                    _ => &[],
                })
                .map(|summary| summary.text.as_str())
                .collect(),
            Self::Completions(message) => message.reasoning_text(),
        }
    }

    /// Calls in provider order, normalized for local execution. Responses keeps its
    /// exact argument string as authority and parses it only for this derived view.
    pub fn tool_calls(&self) -> Result<Vec<ToolCall>, serde_json::Error> {
        match self {
            Self::Messages(blocks) => Ok(blocks
                .iter()
                .filter_map(|block| match block {
                    MessagesOutputBlock::ToolUse(call) => Some(call.clone()),
                    _ => None,
                })
                .collect()),
            Self::Responses(items) => items
                .iter()
                .filter_map(|item| match item {
                    ResponsesOutputItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } => Some((call_id, name, arguments)),
                    _ => None,
                })
                .map(|(call_id, name, arguments)| {
                    Ok(ToolCall::new(
                        name.clone(),
                        call_id.0.clone(),
                        serde_json::from_str(arguments)?,
                    ))
                })
                .collect(),
            Self::Completions(message) => message.tool_calls(),
        }
    }
}

/// One ordered content block in an Anthropic-Messages assistant message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessagesOutputBlock {
    Text(String),
    /// Visible thinking plus the opaque signature required for replay.
    Thinking {
        text: String,
        signature: String,
    },
    /// Safety-redacted thinking whose only replay representation is opaque data.
    RedactedThinking {
        data: String,
    },
    ToolUse(ToolCall),
}

/// One ordered output item returned by OpenAI Responses.
///
/// The first integration exposes custom function tools only, so these are the output
/// forms it can intentionally produce. `Unsupported` retains a future or unexpected
/// item for audit; an assembler that constructs it marks the enclosing turn record-only
/// rather than projecting or dispatching it (docs/transport.md).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResponsesOutputItem {
    Reasoning {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// Visible `summary_text` parts. May be empty even when the model reasoned.
        #[serde(default)]
        summary: Vec<ResponsesReasoningSummary>,
        /// Visible `reasoning_text` parts; absent for most models. Replayed when
        /// present so the item returns as the provider emitted it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ResponsesReasoningText>,
        /// Opaque continuation state. Populated by default in stateless mode
        /// (`store: false`); complete only on `response.output_item.done`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    Message {
        id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<ResponsesPhase>,
        content: Vec<ResponsesMessageContent>,
    },
    FunctionCall {
        /// The output item's identity (`fc_...`), distinct from the call/result join
        /// key. Optional on the wire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        call_id: CallId,
        name: String,
        /// The provider's exact JSON-encoded argument string. This is the replay
        /// authority; execution parses it on demand rather than storing a second copy.
        arguments: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    Unsupported(Value),
}

/// One `summary_text` part of a reasoning item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesReasoningSummary {
    pub text: String,
}

/// One `reasoning_text` part of a reasoning item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesReasoningText {
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesPhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ResponsesMessageContent {
    OutputText {
        text: String,
        #[serde(default)]
        annotations: Vec<Value>,
    },
    Refusal {
        refusal: String,
    },
}
