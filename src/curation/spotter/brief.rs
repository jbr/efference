//! A structured tool's brief as the spotter reads it (docs/permissions.md): where the
//! tool came from, what it says it does, what it guarantees for every input, and what it
//! accepts, each whole: a clipped schema can hide the description of an argument the
//! call uses.

use crate::tools::ToolBrief;
use mcplease::types::Tool as ToolDefinition;

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
        Some(description) => s.push_str(&format!("\nWhat it says it does:\n{description}\n")),
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
    s.push_str(&format!("\nWhat it accepts:\n{schema}\n"));
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
