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
        "move": proposed_move(role, call, brief, cwd, scratch),
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
            let access = access(perms);
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

fn access(perms: &crate::config::Perms) -> &'static str {
    match (perms.read, perms.write) {
        (_, true) => "read, write",
        (true, false) => "read only",
        (false, false) => "denied",
    }
}

fn watch_for(role: &Config) -> Vec<&str> {
    role.watch_for
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The paths a shell command names, resolved, each with the grant that decides it
/// (`policy::path_words`) — the resolution `path_outside`, `path_denied`, and
/// `in_scope` otherwise do inside the model, as a multi-hop read of the command
/// text. Stated as data beside the command,
/// the same facts the prose rung's move block carries.
fn paths(
    command: &str,
    role: &Config,
    cwd: &std::path::Path,
    scratch: Option<&std::path::Path>,
) -> (Vec<Value>, bool) {
    let found = crate::policy::path_words(command, cwd, scratch);
    let entries = found
        .words
        .into_iter()
        .map(|word| {
            use crate::policy::Decided;
            let mut entry = match (&word.resolved, crate::policy::decide(role, &word, cwd, scratch)) {
                // Listed rather than dropped: a list missing it reads as complete
                // (docs/permissions.md).
                (None, _) | (_, Decided::Unresolved) => json!({
                    "as_written": word.word,
                    "resolved": null,
                    "expands_at_run_time": true,
                }),
                (Some(resolved), decided) => {
                    let mut entry = json!({
                        "as_written": word.word,
                        "resolved": resolved.display().to_string(),
                    });
                    let object = entry.as_object_mut().expect("built as an object");
                    match decided {
                        Decided::Grant(grant, perms) => {
                            object.insert(
                                "under_grant".into(),
                                json!({ "path": grant.display().to_string(), "access": access(perms) }),
                            );
                        }
                        Decided::NoGrant | Decided::Unresolved => {
                            object.insert("under_grant".into(), Value::Null);
                        }
                        Decided::Straddles(_) => {
                            object.insert("grant_undetermined".into(), json!(true));
                        }
                    }
                    if word.pattern {
                        object.insert("pattern".into(), json!(true));
                    }
                    entry
                }
            };
            if word.written {
                entry
                    .as_object_mut()
                    .expect("built as an object")
                    .insert("written".into(), json!(true));
            }
            entry
        })
        .collect();
    (entries, found.complete)
}

/// The move itself. Bash carries its command text and nothing else — the text is
/// self-describing, which is exactly what a bare structured call is not, so a
/// structured call carries what the tool *is* alongside what it was handed.
fn proposed_move(
    role: &Config,
    call: &ToolCall,
    brief: Option<&ToolBrief<'_>>,
    cwd: Option<&std::path::Path>,
    scratch: Option<&std::path::Path>,
) -> Value {
    if call.name == bash::NAME {
        return match call.input.get("command").and_then(Value::as_str) {
            Some(command) => {
                // A leading `cd` into the working directory goes nowhere, so the move
                // is the rest (`policy::without_cd_into`) — the resolution the model
                // would otherwise have to do against `working_directory` itself.
                let mut shell = json!({
                    "kind": "shell_command",
                    "command": cwd
                        .and_then(|cwd| crate::policy::without_cd_into(command, cwd))
                        .unwrap_or(command),
                });
                if let Some((paths, complete)) = cwd.map(|cwd| paths(command, role, cwd, scratch))
                    && !paths.is_empty()
                {
                    let object = shell.as_object_mut().expect("built as an object");
                    object.insert("paths".into(), Value::Array(paths));
                    if !complete {
                        object.insert("paths_complete".into(), json!(false));
                    }
                }
                shell
            }
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
