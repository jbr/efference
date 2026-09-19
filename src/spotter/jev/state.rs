//! The `state` a decisions request carries — the whole of what the model reads
//! (docs/permissions.md).
//!
//! The prose spotter renders its context and its move as English, because a
//! generative model reads English. A decision model is answering fixed questions
//! *about* a state, and the route takes that state as JSON, so this renders
//! structure: named fields the question text can point at by name
//! (`path_grants`, `working_directory`, `watch_for`).
//!
//! What is included and what is withheld follows the prose spotter exactly — the
//! same grants including the denies, the same `$TMPDIR` alias, the same resolved
//! absolute paths so containment is visible without the model resolving anything,
//! and the harness-injected `intent` withheld for the same reason (the party being
//! judged wrote it).

use serde_json::{Value, json};

use crate::bash;
use crate::config::Config;
use crate::log::ToolCall;
use crate::tools::ToolBrief;

/// The state for one assessment.
pub(super) fn state(
    role: &Config,
    call: &ToolCall,
    brief: Option<&ToolBrief<'_>>,
    chain: &[String],
    probe: Option<&str>,
    cwd: Option<&std::path::Path>,
    scratch: Option<&std::path::Path>,
) -> Value {
    let mut state = json!({
        "mission": role
            .scope
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        "working_directory": cwd.map(|p| p.display().to_string()),
        "path_grants": grants(role, cwd),
        "scratch_directory": scratch.map(|p| p.display().to_string()),
        "watch_for": watch_for(role),
        "move": proposed_move(call, brief),
    });
    let object = state.as_object_mut().expect("state is built as an object");
    if let Some(holder) = chain.last() {
        // The leaf of an approval chain (docs/permissions.md): the move is the
        // child's, and the chain names who approved on the way here. Present only
        // when there is one, so an ordinary move's state carries no empty field
        // inviting an answer about it.
        object.insert(
            "approval_chain".into(),
            json!({
                "sessions": chain,
                "held_by": holder,
            }),
        );
    }
    if let Some(prefix) = probe {
        object.insert("promotion_probe".into(), json!(prefix));
    }
    state
}

/// The path grants, resolved absolute by the same lexical rule the deterministic
/// rung applies (`src/policy/paths.rs`), so containment against
/// `working_directory` is string-visible and the model is never asked to resolve
/// the grant side. The configured spelling rides along where it differs, so a move
/// that types `../x` literally still matches something in the list.
///
/// The denies are included: a more specific grant overrides a broader one, and a
/// denied subtree under an allowed parent is exactly the carve-out a bash move can
/// stray into without the deterministic fast-path ever seeing it.
fn grants(role: &Config, cwd: Option<&std::path::Path>) -> Vec<Value> {
    role.paths
        .iter()
        .map(|(path, perms)| {
            let access = match (perms.read, perms.write) {
                (_, true) => "read, write",
                (true, false) => "read only",
                (false, false) => "denied",
            };
            let resolved = cwd.map(|cwd| {
                crate::policy::lexical_normalize(&crate::policy::absolutize(path, cwd))
                    .display()
                    .to_string()
            });
            match resolved {
                Some(resolved) if resolved != *path => json!({
                    "path": resolved,
                    "access": access,
                    "granted_as": path,
                }),
                Some(resolved) => json!({ "path": resolved, "access": access }),
                None => json!({ "path": path, "access": access }),
            }
        })
        .collect()
}

fn watch_for(role: &Config) -> Vec<&str> {
    role.watch_for
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The move itself. Bash carries its command text and nothing else — the text is
/// self-describing, which is exactly what a bare structured call is not, so a
/// structured call carries what the tool *is* alongside what it was handed.
fn proposed_move(call: &ToolCall, brief: Option<&ToolBrief<'_>>) -> Value {
    if call.name == bash::NAME {
        return match call.input.get("command").and_then(Value::as_str) {
            Some(command) => json!({ "kind": "shell_command", "command": command }),
            None => json!({
                "kind": "malformed_call",
                "tool": call.name,
                "input": call.input,
                "note": "a shell call carrying no string command",
            }),
        };
    }
    // The harness-injected `intent` is withheld (docs/tools.md, docs/permissions.md):
    // the agent authoring it is the party being judged, so any weight put on it is
    // a channel for talking past the gate on assertion alone.
    let mut input = call.input.clone();
    if let Some(object) = input.as_object_mut() {
        object.remove(crate::tools::INTENT);
    }
    let mut proposed = json!({
        "kind": "tool_call",
        "tool": crate::mcp::display_name(&call.name),
        "input": input,
    });
    let object = proposed.as_object_mut().expect("built as an object");
    if let Some(brief) = brief {
        let definition = brief.definition;
        object.insert(
            "tool_source".into(),
            match brief.server {
                Some(server) => json!(format!(
                    "the connected MCP server `{server}`, which this operator declared and \
                     this harness does not implement or vet"
                )),
                None => json!("built into this harness"),
            },
        );
        if let Some(title) = definition.title.as_deref().filter(|t| !t.trim().is_empty()) {
            object.insert("tool_title".into(), json!(title));
        }
        if let Some(description) = definition
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            object.insert(
                "tool_description".into(),
                json!(super::super::clip(
                    description,
                    super::super::DESCRIPTION_BUDGET
                )),
            );
        }
        let declarations = super::super::declarations(definition);
        if !declarations.is_empty() {
            // The server's claims about its own tool, labelled as such: enough to
            // raise a concern, never enough on its own to settle one
            // (docs/permissions.md — the threat model is error, not deception).
            object.insert(
                "tool_declarations".into(),
                json!({
                    "self_reported": brief.server.is_some(),
                    "claims": declarations,
                }),
            );
        }
        object.insert(
            "tool_input_schema".into(),
            json!(super::super::clip(
                &serde_json::to_string(&definition.input_schema)
                    .unwrap_or_else(|_| definition.input_schema.to_string()),
                super::super::SCHEMA_BUDGET
            )),
        );
    }
    proposed
}
