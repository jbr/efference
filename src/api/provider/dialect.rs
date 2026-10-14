//! The wire dialect a provider speaks (docs/transport.md).

use super::*;

/// The request/response shape a provider speaks (docs/transport.md). Derived
/// from the declared provider — there is no independent knob — so one session
/// projects and assembles in exactly one dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDialect {
    /// Anthropic Messages: `system` + `messages[]`, content blocks, SSE
    /// `message_start` … `message_stop`.
    Messages,
    /// OpenAI Responses: `input[]` items, ordered output items, SSE
    /// `response.created` … `response.completed`.
    Responses,
    /// Chat Completions: `messages[]` with `role: "tool"` results, assistant
    /// messages replayed as received, SSE `choices[0].delta` chunks ending in
    /// `[DONE]` (backlog/completions-dialect.md until it is served).
    Completions,
    /// The System One contract (docs/transport.md): one POST of
    /// `{model, state, questions}` answered with a probability per question.
    /// Not a turn dialect — it has no assistant output to encode or assemble —
    /// so [`Provider::inference_dialect`] never returns it, and a connection
    /// speaking it can back the spotter role only.
    SystemOne,
}

impl WireDialect {
    /// The longest tool name this dialect's request accepts. Both dialects share
    /// the alphabet `[a-zA-Z0-9_-]`; only the ceiling differs — Messages allows 128
    /// characters, Responses 64 (docs/tools.md).
    pub fn tool_name_limit(self) -> usize {
        match self {
            Self::Messages => 128,
            Self::Responses => 64,
            // OpenAI's function-name limit, which the other Completions servers
            // inherit or exceed.
            Self::Completions => 64,
            // No tools ride a System One request; the limit is never consulted.
            Self::SystemOne => 0,
        }
    }

    /// Why `name` is not a legal tool name on this dialect, or `None` if it is. The
    /// check runs over the frozen toolbox at boot (docs/tools.md), so an illegal name
    /// fails the session before a request carries it and the provider answers 400.
    pub fn tool_name_problem(self, name: &str) -> Option<String> {
        let limit = self.tool_name_limit();
        if name.is_empty() {
            return Some("is empty".into());
        }
        if name.len() > limit {
            return Some(format!(
                "is {} characters; the {self} dialect allows at most {limit}",
                name.len()
            ));
        }
        name.chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
            .map(|c| format!("contains {c:?}; the {self} dialect allows only [a-zA-Z0-9_-]"))
    }

    /// The name configuration spells this dialect with — the inverse of
    /// [`Self::from_name`], for writing a connection back out.
    pub fn config_name(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::Responses => "responses",
            Self::Completions => "completions",
            Self::SystemOne => "systemone",
        }
    }

    /// Parse a configured dialect name, for a described endpoint
    /// (docs/transport.md). The vocabulary is closed: a dialect is an encoder and
    /// an assembler in this crate, so an unrecognized name has nothing to select.
    pub fn from_name(value: &str) -> Result<Self, Error> {
        match value.trim() {
            "messages" => Ok(Self::Messages),
            "responses" => Ok(Self::Responses),
            "completions" => Ok(Self::Completions),
            "systemone" => Ok(Self::SystemOne),
            other => Err(format!(
                "dialect {other:?} not recognized: valid values are `messages` \
                 (Anthropic Messages), `responses` (OpenAI Responses), `completions` \
                 (Chat Completions), and `systemone` (System One decision models) \
                 (docs/transport.md)"
            )
            .into()),
        }
    }
}

impl std::fmt::Display for WireDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Messages => "Messages",
            Self::Responses => "Responses",
            Self::Completions => "Chat Completions",
            Self::SystemOne => "System One",
        })
    }
}
