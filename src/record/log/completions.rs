//! A Chat Completions assistant message, stored as received (docs/transport.md).
//!
//! The harness never generates reasoning, and vendors disagree on where it arrives
//! (`reasoning_content`, `reasoning` / `reasoning_details`, `thinking` content parts).
//! So the message is kept as the stream assembled it, every key under the name it
//! arrived on, and replayed under the same keys. The accessors here read the keys
//! the dialect defines (`content`, `tool_calls`, `refusal`) and read everything else
//! only for display.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::ToolCall;

#[cfg(test)]
mod test;

/// The keys the dialect defines. Every other key is opaque: replayed, and read
/// only by the display heuristic in [`CompletionsMessage::reasoning_text`].
const DEFINED: [&str; 4] = ["role", "content", "tool_calls", "refusal"];

/// Keys whose string values a display reads out of an opaque structure.
const READABLE: [&str; 4] = ["text", "summary", "content", "thinking"];

/// One assistant message. Serializes as the bare object it replays as.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct CompletionsMessage(Map<String, Value>);

impl CompletionsMessage {
    /// A message exactly as given.
    pub fn from_map(message: Map<String, Value>) -> Self {
        Self(message)
    }

    /// The message an assembled stream produced. Drops what belongs to the stream
    /// rather than the message: `null` values (a server's way of saying absent),
    /// `role` (the encoder writes it as the message's tag), and each tool call's
    /// `index`, which orders fragments and is not part of the request schema.
    pub fn from_assembled(mut message: Map<String, Value>) -> Self {
        message.retain(|key, value| !value.is_null() && key != "role");
        if let Some(Value::Array(calls)) = message.get_mut("tool_calls") {
            for call in calls.iter_mut().filter_map(Value::as_object_mut) {
                call.remove("index");
            }
        }
        Self(message)
    }

    /// The object as it replays.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// For the secret mask (docs/env.md), which rewrites values in place.
    pub(crate) fn as_map_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.0
    }

    /// No output: nothing besides `role`, or an empty `content` alone.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|(key, value)| match key.as_str() {
            "role" => true,
            "content" => value.as_str().is_some_and(str::is_empty),
            _ => false,
        })
    }

    /// Visible prose in order: `content` as a string or its `text` parts, then a
    /// `refusal`.
    pub fn text(&self) -> Vec<&str> {
        let mut text = Vec::new();
        match self.0.get("content") {
            Some(Value::String(content)) => text.push(content.as_str()),
            Some(Value::Array(parts)) => text.extend(
                parts
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str)),
            ),
            _ => {}
        }
        text.extend(self.0.get("refusal").and_then(Value::as_str));
        text.retain(|line| !line.is_empty());
        text
    }

    /// Readable reasoning, by heuristic, for display only — it never reaches the
    /// wire. A top-level opaque key with a string value is reasoning text
    /// (`reasoning_content`, OpenRouter's `reasoning`). Only when there is none are
    /// structured keys and non-`text` content parts read, through the strings
    /// under [`READABLE`] keys: OpenRouter sends the same reasoning both as
    /// `reasoning` and inside `reasoning_details`, and showing both would double it.
    pub fn reasoning_text(&self) -> Vec<&str> {
        let opaque = || {
            self.0
                .iter()
                .filter(|(key, _)| !DEFINED.contains(&key.as_str()))
        };
        let flat: Vec<&str> = opaque()
            .filter_map(|(_, value)| value.as_str())
            .filter(|text| !text.is_empty())
            .collect();
        if !flat.is_empty() {
            return flat;
        }
        let mut text = Vec::new();
        for (_, value) in opaque() {
            readable(value, &mut text);
        }
        if let Some(Value::Array(parts)) = self.0.get("content") {
            for part in parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) != Some("text"))
            {
                readable(part, &mut text);
            }
        }
        text.retain(|line| !line.is_empty());
        text
    }

    /// Calls in order, normalized for local execution. The `arguments` string is
    /// the replay authority and is parsed only for this view; an empty string is
    /// a call with no arguments.
    pub fn tool_calls(&self) -> Result<Vec<ToolCall>, serde_json::Error> {
        let Some(Value::Array(calls)) = self.0.get("tool_calls") else {
            return Ok(Vec::new());
        };
        calls
            .iter()
            .map(|call| {
                let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                let function = call.get("function");
                let field = |name| {
                    function
                        .and_then(|f| f.get(name))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                };
                let arguments = field("arguments");
                let input = if arguments.trim().is_empty() {
                    Value::Object(Map::new())
                } else {
                    serde_json::from_str(arguments)?
                };
                Ok(ToolCall::new(field("name"), id, input))
            })
            .collect()
    }
}

/// The strings under [`READABLE`] keys inside `value`, depth first in key order.
fn readable<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| readable(item, out)),
        Value::Object(map) => {
            for (key, value) in map {
                match value {
                    Value::String(text) if READABLE.contains(&key.as_str()) => out.push(text),
                    _ => readable(value, out),
                }
            }
        }
        _ => {}
    }
}
