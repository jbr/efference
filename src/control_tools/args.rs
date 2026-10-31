//! Argument shapes the verbs share, and the sender a call over MCP is named by.
//!
//! The `agent` argument is declared here once for each form it takes: required, or
//! optional with what an omitted one means. A verb holds the form as its `agent` field.
//! Each form is a newtype serde reads as the bare id, so the verb's field deserializes
//! exactly as a plain `String` or `Option<String>` field would; the schema describes the
//! field from the form's own field, its description included; and the command line
//! flattens the form in as `--agent` (docs/tools.md). So a description, or what an
//! omitted agent means, is written once for every verb that shares the form.
//!
//! None of the forms carries a `///` doc of its own: a newtype's doc would replace the
//! argument's description in the schema, and an `Args` struct's doc becomes the `about`
//! of the command it is flattened into.

use mcplease::types::RequestContext;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

// The agent a verb acts on, which every call names: `message`, `decide`, `interrupt` and
// `transcript` take it so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(transparent)]
pub struct NamedAgent {
    /// The agent's id.
    #[arg(long)]
    agent: String,
}

impl NamedAgent {
    /// The agent's id.
    pub fn id(&self) -> &str {
        &self.agent
    }
}

impl From<&str> for NamedAgent {
    fn from(agent: &str) -> Self {
        Self {
            agent: agent.to_string(),
        }
    }
}

impl From<String> for NamedAgent {
    fn from(agent: String) -> Self {
        Self { agent }
    }
}

// The agent an operator's route verb drives, which the call may name: omitted, the one
// session the host lists in the directory the command runs in (`Fallback::Discover`).
// A verb holds it under `#[serde(default, skip_serializing_if = "AgentOrHere::is_none")]`:
// a newtype is no `Option` to the schema, so `default` marks the field optional there,
// and the skip keeps an absent agent from being shown as a `null` default.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, clap::Args,
)]
#[serde(transparent)]
pub struct AgentOrHere {
    /// The agent's id. Defaults to the one session live here.
    #[arg(long)]
    agent: Option<String>,
}

impl AgentOrHere {
    /// The agent's id, when the call names one.
    pub fn id(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Whether the call names no agent.
    pub fn is_none(&self) -> bool {
        self.agent.is_none()
    }
}

// The agent `status` reads, which the call may name: omitted, every agent in view. Held
// as `AgentOrHere` is, and for the same reason. The command line's `status` repeats
// `--agent` instead, so this form has no flag.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct AgentOrAll {
    /// The agent's id; omitted, every agent in view.
    agent: Option<String>,
}

impl AgentOrAll {
    /// The agent's id, when the call names one.
    pub fn id(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Whether the call names no agent.
    pub fn is_none(&self) -> bool {
        self.agent.is_none()
    }
}

impl From<Option<String>> for AgentOrAll {
    fn from(agent: Option<String>) -> Self {
        Self { agent }
    }
}

impl From<&str> for AgentOrAll {
    fn from(agent: &str) -> Self {
        Some(agent.to_string()).into()
    }
}

/// A transcript handle given as an argument: `"8.2"`, or a seq an agent wrote as a
/// number.
pub(crate) fn handle<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(handle) => Ok(handle),
        Value::Number(seq) => Ok(seq.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "seq is a transcript handle like \"8\" or \"8.2\", not {other}"
        ))),
    }
}

/// [`handle`] for an optional argument.
pub(crate) fn optional_handle<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(handle)) => Ok(Some(handle)),
        Some(Value::Number(seq)) => Ok(Some(seq.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "a transcript handle is like \"8\" or \"8.2\", not {other}"
        ))),
    }
}

/// The handle a call's input names under `key`, as [`handle`] reads it.
pub(crate) fn handle_in(input: &Value, key: &str) -> Option<String> {
    match input.get(key)? {
        Value::String(handle) => Some(handle.clone()),
        Value::Number(seq) => Some(seq.to_string()),
        _ => None,
    }
}

/// The `source` a call over MCP carries (docs/control-api.md): the caller named from
/// the `clientInfo` it sent, so a mounting harness is named without saying anything.
/// `None` (the command line's empty context, or a caller that declared no name) means
/// the operator, whose turns render no prefix.
pub(crate) fn turn_source(context: &RequestContext) -> Option<String> {
    context
        .client_info()
        .map(|info| format!("mcp[{}]", info.name))
}

/// The reader a call over MCP is, for a status or a transcript that names it "you":
/// the client's name, as its turns' `source` carries it.
pub(crate) fn reader_name(context: &RequestContext) -> Option<String> {
    context.client_info().map(|info| info.name.to_string())
}
