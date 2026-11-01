//! What a described connection says about its request body beyond the dialect
//! (docs/transport.md, "The Chat Completions dialect"). Vendors serving the same
//! dialect spell a few fields differently; these settings name the behaviour, not
//! the vendor, so one connection block describes any of them.

use serde_json::{Map, Value};

use crate::config::user_config::OutputCapField;

#[cfg(test)]
mod test;

/// Per-connection request settings. Fixed for the session, so they cannot move a
/// cached prefix mid-session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RequestOptions {
    /// Fields merged into every request body, for vendor parameters whose value
    /// never changes (`parallel_tool_calls`, `safe_prompt`). A key the encoder
    /// writes is refused at load ([`RequestOptions::new`]).
    pub body: Map<String, Value>,
    /// The key the output cap travels under.
    pub output_cap_field: OutputCapField,
}

/// Keys the Completions encoder writes, which a `body` table may not replace. The
/// effort has its own knob (`--effort`, `[transport] effort`), which the encoder
/// sends as `reasoning_effort`, so a fixed copy in `body` would shadow it.
const ENCODER_KEYS: [&str; 10] = [
    "model",
    "messages",
    "tools",
    "tool_choice",
    "stream",
    "stream_options",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "reasoning_effort",
];

impl RequestOptions {
    /// Validate a connection's settings. `body` refuses the keys the encoder
    /// writes, and `output_cap_field` names one of the two spellings.
    pub fn new(body: Map<String, Value>, output_cap_field: Option<&str>) -> Result<Self, String> {
        if let Some(key) = body.keys().find(|key| ENCODER_KEYS.contains(&key.as_str())) {
            let hint = match key.as_str() {
                "reasoning_effort" => "; set the effort with `--effort` or `[transport] effort`",
                "max_tokens" | "max_completion_tokens" => {
                    "; choose the cap's spelling with `output-cap-field`"
                }
                _ => "",
            };
            return Err(format!(
                "`body` may not set `{key}`: the request encoder writes it{hint}"
            ));
        }
        let output_cap_field = match output_cap_field {
            None | Some("max_tokens") => OutputCapField::MaxTokens,
            Some("max_completion_tokens") => OutputCapField::MaxCompletionTokens,
            Some(other) => {
                return Err(format!(
                    "output-cap-field {other:?} not recognized: valid values are \
                     `max_tokens` and `max_completion_tokens`"
                ));
            }
        };
        Ok(Self {
            body,
            output_cap_field,
        })
    }
}
