//! Argument shapes the verbs share, and the sender a call over MCP is named by.

use mcplease::types::RequestContext;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

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
