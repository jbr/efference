//! The wire dialect a provider speaks (docs/transport.md): the behavior of
//! [`WireDialect`], which the log defines because an assistant turn is recorded in its arm.

use super::*;

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

    /// Every name [`Self::from_name`] accepts, with the hover text the published user
    /// schema shows for it (docs/config-schemas.md).
    pub(super) const NAMED: &[(&str, Self, &str)] = &[
        ("messages", Self::Messages, "Anthropic Messages."),
        ("responses", Self::Responses, "OpenAI Responses."),
        ("completions", Self::Completions, "OpenAI Chat Completions."),
        (
            "systemone",
            Self::SystemOne,
            "TypeSafe's System One decision models, which can serve as the spotter only.",
        ),
    ];

    /// Parse a configured dialect name, for a described endpoint
    /// (docs/transport.md). The vocabulary is closed: a dialect is an encoder and
    /// an assembler in this crate, so an unrecognized name has nothing to select.
    pub fn from_name(value: &str) -> Result<Self, Error> {
        let value = value.trim();
        Self::NAMED
            .iter()
            .find(|(name, ..)| *name == value)
            .map(|(_, dialect, _)| *dialect)
            .ok_or_else(|| {
                let valid = Self::NAMED
                    .iter()
                    .map(|(name, _, doc)| format!("`{name}` ({})", doc.trim_end_matches('.')))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("dialect {value:?} not recognized: valid values are {valid}").into()
            })
    }

    /// The published user schema for a connection's `dialect` (docs/config-schemas.md).
    pub fn name_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        crate::config_schemas::documented_enum(
            Self::NAMED.iter().map(|(name, _, doc)| (*name, *doc)),
        )
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
