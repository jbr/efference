//! What has arrived of a tool call's JSON input, mid-stream (docs/tui.md).
//!
//! A tool call's arguments arrive as fragments of a JSON document, so at any moment
//! the accumulated buffer is a *prefix* of one — `{"path":"src/api.rs","contents":"use st`.
//! Rendering that prefix as fields rather than as characters is what lets the TUI
//! show a file being written while it is being written, instead of a wall of
//! escaped JSON.
//!
//! The rule that keeps it honest is **positional**: within an object, only the
//! **last** key can be incomplete, because the bytes for the earlier ones already
//! arrived and closed. So the settled pairs may be rendered as identity — this call
//! *is* a write to `src/api.rs` — while the last pair is still arriving and may only
//! be rendered as content. Rendering a half-arrived `"src/api.r` as a path would be
//! a well-formed, plausible, false claim, which is worse than showing raw bytes.
//!
//! The split is read off the parser rather than guessed: jiter's
//! [`PartialMode::On`] drops an incomplete trailing value, and
//! [`PartialMode::TrailingStrings`] keeps it, so the one key the second has and the
//! first lacks is exactly the value still in flight.

#[cfg(test)]
mod test;

use jiter::{JsonValue, PartialMode};
use serde_json::Value;

/// A tool call's input as far as it has arrived.
#[derive(Debug, Default, PartialEq)]
pub(in crate::tui) struct PartialInput {
    /// Complete key/value pairs in **arrival order** — the order the model emitted
    /// them, which `serde_json::Value` cannot hold (it sorts, for the wire's byte
    /// stability — docs/log-and-projection.md). Every value here is final.
    pub settled: Vec<(String, Value)>,
    /// The pair still arriving: its key, and how much of its value has decoded.
    /// `None` once the document closes, and for a buffer that ends on a pair
    /// boundary.
    pub arriving: Option<(String, Value)>,
}

impl PartialInput {
    /// Read `buffer` — a prefix of a JSON object — into settled and arriving pairs.
    ///
    /// `None` when nothing structural has arrived yet (`{"pa`, or an empty buffer):
    /// the caller falls back to showing the raw characters, which claim nothing.
    /// `None` also for a top-level value that isn't an object, since a tool call's
    /// input always is one and anything else is not a case to invent a rendering for.
    pub(in crate::tui) fn parse(buffer: &str) -> Option<Self> {
        let bytes = buffer.as_bytes();
        // The trailing-strings parse is the fuller of the two; if even it cannot
        // make an object out of the prefix, there is nothing structural to show.
        let loose = object_pairs(bytes, PartialMode::TrailingStrings)?;
        // Whatever the strict parse also has is closed. It can fail where the loose
        // one succeeds (a buffer that is *only* an incomplete pair), which just
        // means nothing has settled yet.
        let settled_len = object_pairs(bytes, PartialMode::On).map_or(0, |pairs| pairs.len());

        let mut pairs = loose;
        // Only the final pair can be incomplete, so anything the strict parse
        // dropped is that pair and nothing else.
        let arriving = (pairs.len() > settled_len).then(|| pairs.pop()).flatten();
        Some(Self {
            settled: pairs,
            arriving,
        })
    }

    /// The settled pairs as a `serde_json::Value` — what the per-tool argument
    /// renderer consumes. Key order is lost here (the map sorts), which is fine
    /// because that renderer looks keys up by name; order matters only to
    /// [`parse`](Self::parse), for deciding which pair is in flight.
    ///
    /// The arriving pair is deliberately **not** included: this is the value that
    /// gets rendered as identity, and only closed values may be.
    pub(in crate::tui) fn settled_value(&self) -> Value {
        Value::Object(self.settled.iter().cloned().collect())
    }

    /// The arriving value as text, when it is a string — the case worth rendering
    /// as content (a file's contents, a long command, a message body). A partial
    /// number or container has no useful intermediate form and is left to the
    /// caller to summarize.
    pub(in crate::tui) fn arriving_text(&self) -> Option<(&str, &str)> {
        let (key, value) = self.arriving.as_ref()?;
        Some((key.as_str(), value.as_str()?))
    }
}

/// Parse `bytes` at `mode` and return its top-level object's pairs in order, or
/// `None` if it isn't a (possibly partial) object.
fn object_pairs(bytes: &[u8], mode: PartialMode) -> Option<Vec<(String, Value)>> {
    match JsonValue::parse_owned(bytes, false, mode) {
        Ok(JsonValue::Object(entries)) => Some(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), convert(value)))
                .collect(),
        ),
        _ => None,
    }
}

/// jiter's value into serde's. A conversion rather than a reuse because the rest of
/// the projection — and the per-tool argument renderer in particular — is written
/// against `serde_json::Value`, which is also what a *committed* call carries.
/// Rendering both through one type is what keeps the live block and the committed
/// block from drifting apart.
fn convert(value: &JsonValue<'_>) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Int(i) => Value::from(*i),
        JsonValue::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
        JsonValue::Str(s) => Value::String(s.to_string()),
        JsonValue::Array(items) => Value::Array(items.iter().map(convert).collect()),
        JsonValue::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), convert(value)))
                .collect(),
        ),
    }
}
