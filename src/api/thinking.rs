//! Extended-thinking selection per model, and the tool-choice knob (docs/transport.md).

use super::*;

/// The `thinking` field (docs/transport.md). `Adaptive` lets the model decide
/// when and how much to think; `display: "summarized"` captures a readable
/// model-authored summary into the log at the same token cost as omitting it.
/// `Disabled` and `BetweenTools` are the two spellings of thinking off a
/// model's metadata can call for ([`ThinkingOff`]). `Omit` leaves the field off
/// the request entirely (via the `Request` field's `skip_serializing_if`), the
/// spelling for a source that declares nothing, where an explicit value is an
/// assertion a mandatory-reasoning endpoint rejects.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Thinking {
    Disabled,
    BetweenTools,
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
    },
    /// Never serialized — `is_omit` gates the field off the request.
    Omit,
}

impl Thinking {
    pub(super) fn is_omit(&self) -> bool {
        matches!(self, Self::Omit)
    }

    /// The `thinking` a request carries, from the model's [`ThinkingSupport`],
    /// the `tool_choice`, and whether the client asked for thinking off
    /// (`Client::without_thinking`). An `auto` turn thinks adaptively where the
    /// model accepts it; everything else — a forced choice (Anthropic answers
    /// forced choice with thinking on with a 400), a client built without
    /// thinking, a model without adaptive — takes the model's spelling of off.
    ///
    /// The keepalive calls this too, with the turn's own `tool_choice`: a ping
    /// whose thinking differs from the turn's misses the message-level cache on
    /// Anthropic and writes a fresh entry instead of refreshing the live one.
    pub(super) fn for_request(
        tool_choice: &ToolChoice,
        support: ThinkingSupport,
        thinking_off: bool,
    ) -> Self {
        if tool_choice.kind == "auto" && !thinking_off && support.adaptive {
            THINKING_ADAPTIVE
        } else {
            support.off.wire()
        }
    }
}

/// What a model's metadata says about the `thinking` field (docs/transport.md):
/// whether an `auto` turn may think adaptively, and how to say "don't think".
/// One answer per model, taken from the capability lookup at boot and never
/// changed, so a session's thinking never moves its cached prefix.
///
/// Anthropic reads `capabilities.thinking.types`; OpenRouter reads whether every
/// serving endpoint lists `reasoning` and the model entry's `reasoning.mandatory`;
/// a source that declares nothing is [`Self::UNKNOWN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ThinkingSupport {
    /// The model accepts `thinking: {type: adaptive}`.
    pub adaptive: bool,
    /// How a request says thinking off.
    pub off: ThinkingOff,
}

impl ThinkingSupport {
    /// A source that declares nothing: no adaptive, and thinking off by
    /// omission, the one spelling that asserts nothing.
    pub const UNKNOWN: Self = Self {
        adaptive: false,
        off: ThinkingOff::Omit,
    };

    /// Whether an `auto` turn on this model sends an explicit thinking-off — the
    /// input to [`check_effort_thinking`]. `Omit` leaves the endpoint to decide,
    /// so effort there is the provider's to answer, not this check's.
    pub fn auto_turns_send_thinking_off(&self) -> bool {
        !self.adaptive && self.off != ThinkingOff::Omit
    }
}

/// How a request says thinking off, per the model's metadata (docs/transport.md).
///
/// `Disabled` is `{type: disabled}`: Anthropic's `thinking.types.disabled`, or an
/// OpenRouter model whose reasoning is optional. `BetweenTools` is
/// `{type: between_tools}`, for an Anthropic model whose metadata refuses
/// `disabled`: the 400 such a model answers `disabled` with names it as the
/// replacement, and the model's short updates between tool calls come back as
/// thinking blocks. The metadata does not list `between_tools` as a type, so the
/// refusal of `disabled` is the signal. `Omit` leaves the field off, for a source
/// that declares nothing or an OpenRouter model whose reasoning is mandatory,
/// where any explicit value is a 400 (OpenRouter's own `reasoning: {enabled:
/// false}` rides beside it when a client asked for thinking off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingOff {
    Disabled,
    BetweenTools,
    Omit,
}

impl ThinkingOff {
    fn wire(self) -> Thinking {
        match self {
            Self::Disabled => Thinking::Disabled,
            Self::BetweenTools => Thinking::BetweenTools,
            Self::Omit => Thinking::Omit,
        }
    }
}

/// The `output_config` object (docs/transport.md) — present on the request only
/// when the session set an effort, so a session that sets none sends the same
/// bytes it always did. One field today; the wrapper exists because the API
/// nests `effort` inside it. The effort is the configured string verbatim: the
/// vocabulary is the model's metadata (`supported_efforts`), not an enum here,
/// so a level the metadata invents rides the wire as spelled.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    pub effort: String,
}

/// An effort level is a string end to end — the flag, the config key, and the
/// wire all carry the configured spelling verbatim (docs/transport.md). The
/// vocabulary is the model's metadata (`ModelCapabilities::supported_efforts`),
/// not an enum here: when a model invents a level, it is validated against the
/// metadata that names it and sent as spelled, not rejected by a local list
/// that cannot know it.
///
/// One coupling with the thinking axis: these levels are legal only with
/// thinking on, and Anthropic answers the pairing with a 400 — `between_tools`
/// counts as off — so [`check_effort_thinking`] rejects it at boot instead. A
/// level beyond these two spellings has no known coupling and passes through.
pub const EFFORTS_REQUIRING_THINKING: [&str; 2] = ["xhigh", "max"];

/// The boot check for the one coupling between the two axes (docs/transport.md):
/// `xhigh`/`max` with thinking off is a 400 from Anthropic, so a session
/// configured that way is refused at startup with a message naming both halves
/// rather than failing on its first turn. `thinking_off` is the session's
/// effective answer, [`ThinkingSupport::auto_turns_send_thinking_off`].
pub fn check_effort_thinking(
    effort: Option<&str>,
    model: &str,
    thinking_off: bool,
) -> Result<(), String> {
    match effort {
        Some(effort) if thinking_off && EFFORTS_REQUIRING_THINKING.contains(&effort) => {
            Err(format!(
                "effort `{effort}` requires thinking, but {model}'s metadata offers no \
                 adaptive thinking, so this session's turns send thinking off — the provider \
                 answers that pairing with a 400. Use a lower level."
            ))
        }
        _ => Ok(()),
    }
}

/// The `tool_choice` field. `auto()` lets the model decide whether to call a
/// tool, and how many to call at once (docs/README.md); `force(name)` makes it
/// call exactly that one, which only the spotter's `spot` does
/// (docs/permissions.md) — forcing moves the prefix relative to the shared
/// envelope every other call type sends. A forced choice also sets
/// `disable_parallel_tool_use`, because its answer is one verdict; `auto` omits
/// the field.
#[derive(Serialize, Clone, Copy)]
pub struct ToolChoice {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'static str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disable_parallel_tool_use: bool,
}

impl ToolChoice {
    pub fn auto() -> Self {
        Self {
            kind: "auto",
            name: None,
            disable_parallel_tool_use: false,
        }
    }

    /// The forced tool's name, when this choice forces one.
    pub fn forced(&self) -> Option<&'static str> {
        self.name
    }

    pub fn force(name: &'static str) -> Self {
        Self {
            kind: "tool",
            name: Some(name),
            disable_parallel_tool_use: true,
        }
    }
}
