//! A structured tool's brief as the spotter reads it (docs/permissions.md): where the
//! tool came from, what it says it does, what it guarantees for every input, and what it
//! accepts.

use crate::tools::ToolBrief;
use mcplease::types::Tool as ToolDefinition;

/// How much of a description and of an input schema ride into the move block. A
/// verbose server (or a schema with a large enum) would otherwise push the fresh
/// tail past the useful signal and pay for it on every gated call. Clipped at a
/// char boundary, with the clip marked so a truncated schema is never mistaken for
/// the whole one.
pub(super) const DESCRIPTION_BUDGET: usize = 1200;
pub(super) const SCHEMA_BUDGET: usize = 2000;

/// Clip to `max` bytes at a char boundary, marking the clip.
pub(super) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated)", &text[..end])
}

/// Describe the tool a structured move would call: where it came from, what it says
/// it does, what it guarantees for every input, and what it accepts.
///
/// **On the behavior hints.** `readOnlyHint` / `destructiveHint` / `idempotentHint` /
/// `openWorldHint` describe the tool over every input it accepts. An unsafe-direction
/// hint (destructive, open-world, not read-only, not idempotent) says only that some
/// input could do harm, and the MCP spec already makes those the defaults when a hint
/// is absent, so it says nothing about *this* call, whose input the spotter can read.
/// When it was rendered, hold reasons cited the tool rather than the call. A safe-direction hint
/// (read-only, additive, idempotent, closed-world) is a guarantee over every input,
/// which the input alone cannot show. Under this harness's threat model, **error, not
/// deception** (docs/permissions.md, *Threat model*), a well-meaning server's guarantee
/// is true, so only [`guarantees`] are shown to the spotter. The caution about tools in
/// general is the persona's stated default (`assets/prompts/spotter.md`).
pub(super) fn render_brief(display: &str, brief: &ToolBrief<'_>) -> String {
    let definition = brief.definition;
    let mut s = match brief.server {
        Some(server) => format!(
            "The tool `{display}`, from the MCP server `{server}`, which this session's \
             configuration connects.\n"
        ),
        None => format!("The tool `{display}`, built into this harness.\n"),
    };
    if let Some(title) = definition.title.as_deref().filter(|t| !t.trim().is_empty()) {
        s.push_str(&format!("Titled: {title}\n"));
    }

    match definition
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(description) => s.push_str(&format!(
            "\nWhat it says it does:\n{}\n",
            clip(description, DESCRIPTION_BUDGET)
        )),
        None => s.push_str("\nIt carries no description.\n"),
    }

    let guarantees = guarantees(definition);
    if !guarantees.is_empty() {
        s.push_str(match brief.server {
            Some(_) => "\nWhat the server guarantees about it, for every input:\n",
            None => "\nWhat the harness guarantees about it, for every input:\n",
        });
        for line in guarantees {
            s.push_str(&format!("- {line}\n"));
        }
    }

    let schema = serde_json::to_string_pretty(&definition.input_schema)
        .unwrap_or_else(|_| definition.input_schema.to_string());
    s.push_str(&format!(
        "\nWhat it accepts:\n{}\n",
        clip(&schema, SCHEMA_BUDGET)
    ));
    s
}

/// The safe-direction behavior hints the tool sets, one line per hint: read-only,
/// additive (`destructiveHint: false`), idempotent and closed-world. The
/// unsafe-direction values and absent hints produce no line; the reasoning is on
/// [`render_brief`]. Shared with the jev state renderer, so one function decides what
/// a hint is allowed to tell either spotter.
pub(super) fn guarantees(definition: &ToolDefinition) -> Vec<&'static str> {
    let Some(hints) = definition.annotations.as_ref() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if hints.read_only_hint == Some(true) {
        lines.push("read-only: it does not modify its environment");
    }
    if hints.destructive_hint == Some(false) {
        lines.push("additive: it does not remove or overwrite what is already there");
    }
    if hints.idempotent_hint == Some(true) {
        lines.push("idempotent: repeating it with the same arguments changes nothing further");
    }
    if hints.open_world_hint == Some(false) {
        lines.push("closed-world: its effects stay inside a closed domain");
    }
    lines
}
