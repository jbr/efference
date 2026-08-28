//! Human-readable renderings of an outbound request — the `wire` dev verb's output.

use super::*;

/// Render the full request envelope as pretty JSON — the dev `wire` view (the
/// `efference wire` subcommand) of exactly what [`Client::complete`] POSTs for a
/// real turn: the same private [`Request`] shape, with `max_tokens`/`stream` set
/// as a streaming turn sends them. The transport headers (`x-api-key`,
/// `anthropic-version`) are request metadata, not part of the body the model
/// reads, so they're omitted. Reusing [`Request`] keeps this faithful by
/// construction: a wire-shape change can't drift the dev view out of sync.
///
/// Serializes the struct directly (not via [`request_value`]) so the top-level
/// field order is preserved — `serde_json::Value` would re-sort the keys, burying
/// `model`/`system` beneath the long `tools`/`messages` blocks.
pub fn render_request_pretty(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    messages: &[ApiMessage],
) -> Result<String, Error> {
    let request = Request {
        model,
        max_tokens: MAX_TOKENS,
        system,
        tools,
        tool_choice,
        thinking: Thinking::for_request(model, &tool_choice, false),
        temperature: None,
        upstream: None,
        messages,
        stream: true,
    };
    Ok(serde_json::to_string_pretty(&request)?)
}

/// Render the same envelope as [`render_request_pretty`] as **pseudo-YAML** (the
/// dev `wire --format yaml` view): a lossy, read-only rendering that unfolds
/// escaped/multi-line strings — tool-schema descriptions, the system prompt,
/// tool-result bodies — into indented blocks instead of `\n`-littered one-liners
/// (see [`crate::pretty`]). Top-level keys are emitted in a hand-picked, readable
/// order (scalars and `system` before the long `tools`/`messages` blocks) rather
/// than the map's sorted order; nested objects follow the sorted default.
pub fn render_request_yaml(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    messages: &[ApiMessage],
) -> Result<String, Error> {
    let value = request_value(model, system, tools, tool_choice, messages);
    let obj = value
        .as_object()
        .expect("request envelope serializes to a JSON object");
    let order = [
        "model",
        "max_tokens",
        "system",
        "tools",
        "tool_choice",
        "thinking",
        "messages",
    ];
    let mut lines = Vec::new();
    for key in order {
        if let Some(v) = obj.get(key) {
            lines.extend(crate::pretty::keyed(key, v));
        }
    }
    Ok(crate::pretty::join(lines))
}

/// Render just the projected `messages` array — the `wire --messages` view: what
/// [`crate::project::project`] produced (with the trailing cache breakpoint
/// placed), skipping the `system`/`tools[]` envelope entirely. Unlike
/// [`render_request_pretty`], this needs no model id, system prompt, or tool
/// list, so it works even when a session's config won't parse (nothing here
/// depends on it) — a lean, config-independent view of what the model sees.
pub fn render_messages_pretty(messages: &[ApiMessage]) -> Result<String, Error> {
    Ok(serde_json::to_string_pretty(messages)?)
}

/// Pseudo-YAML counterpart to [`render_messages_pretty`] (see [`crate::pretty`]
/// for the rendering trade-offs): the projected `messages` array, unfolded for
/// readability, with no `system`/`tools[]` envelope.
pub fn render_messages_yaml(messages: &[ApiMessage]) -> Result<String, Error> {
    let value = serde_json::to_value(messages)?;
    Ok(crate::pretty::to_pseudo_yaml(&value))
}

/// The request envelope as a [`Value`] — the shared source for both dev renderers
/// (`render_request_*`). Reuses the private [`Request`] shape, so a wire-shape
/// change can't drift the dev views out of sync with what `complete` POSTs.
pub(super) fn request_value(
    model: &str,
    system: &str,
    tools: &[Value],
    tool_choice: ToolChoice,
    messages: &[ApiMessage],
) -> Value {
    let request = Request {
        model,
        max_tokens: MAX_TOKENS,
        system,
        tools,
        tool_choice,
        thinking: Thinking::for_request(model, &tool_choice, false),
        temperature: None,
        upstream: None,
        messages,
        stream: true,
    };
    serde_json::to_value(&request).expect("request envelope serializes")
}
