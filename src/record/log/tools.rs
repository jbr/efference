//! The advertised `tools[]` as the log records it (docs/log-and-projection.md,
//! docs/resumption.md): the array a session sent, kept so a resume can say what changed
//! and an offline reader can rebuild the request without the session's MCP servers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{LogEntry, Payload, Telemetry};

/// How a resumed session's `tools[]` differs from the last one recorded, by tool name.
/// Empty when the arrays are equal. `reordered` is set when the names and definitions are
/// the same and only their order moved, which is still a different request prefix.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ToolsChange {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// Same name, different definition (description or schema).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reordered: bool,
}

impl ToolsChange {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && !self.reordered
    }

    /// The difference between two `tools[]` arrays. Definitions compare with `==`:
    /// object keys serialize sorted, so equal tool sets are equal values.
    pub fn between(before: &[Value], now: &[Value]) -> Self {
        let name = |tool: &Value| tool.get("name").and_then(Value::as_str).map(str::to_string);
        let find = |tools: &[Value], wanted: &str| {
            tools
                .iter()
                .find(|tool| name(tool).as_deref() == Some(wanted))
                .cloned()
        };
        let mut change = Self::default();
        for tool in now {
            let Some(tool_name) = name(tool) else {
                continue;
            };
            match find(before, &tool_name) {
                None => change.added.push(tool_name),
                Some(old) if old != *tool => change.changed.push(tool_name),
                Some(_) => {}
            }
        }
        for tool in before {
            let Some(tool_name) = name(tool) else {
                continue;
            };
            if find(now, &tool_name).is_none() {
                change.removed.push(tool_name);
            }
        }
        change.reordered = change.is_empty() && before != now;
        change
    }
}

/// The `tools[]` the latest [`Telemetry::Tools`] entry recorded, or `None` for a log
/// that never recorded one. The latest wins on a multi-boot log; a resume appends an
/// entry only when its array differs, so the latest is the one in force.
pub fn recorded_tools(entries: &[LogEntry]) -> Option<&[Value]> {
    entries.iter().rev().find_map(|entry| match &entry.payload {
        Payload::Telemetry(Telemetry::Tools { tools }) => Some(tools.as_slice()),
        _ => None,
    })
}
