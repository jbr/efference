//! The `state` a decisions request carries — the whole of what the model reads
//! (docs/permissions.md).
//!
//! The prose spotter renders its context and its move as English, because a
//! generative model reads English. A decision model is answering fixed questions
//! *about* a state, and the route takes that state as JSON, so this renders
//! structure: named fields the question text can point at by name
//! (`path_grants`, `working_directory`, `spotter_notes`, `conversation`).
//!
//! What is included and what is withheld follows the prose spotter: the same grants
//! including the denies, the same `$TMPDIR` alias, the same resolved absolute paths so
//! containment is visible without the model resolving anything, the session's
//! conversation with each message's writer, the prose rung's persona as `guidance`,
//! and the harness-injected `intent` withheld for the same reason (the party being
//! judged wrote it).
//!
//! The fields go on the wire in one fixed order ([`State`]): the config, then the
//! guidance and the conversation, then the move. The battery's thresholds were fitted
//! on states in that order.

use std::ops::Index;

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::config::policy::Directories;
use crate::record::log::ToolCall;
use crate::record::names::tools::{BASH, INTENT, display_name};
use crate::record::utterances::{Speaker, Utterances};
use crate::tools::ToolBrief;

use super::super::Circumstances;

/// The state of one assessment: its fields in the order they go on the wire. A
/// `serde_json::Value` object sorts its keys, so the order is carried as pairs and
/// written entry by entry.
#[derive(Clone, Debug, PartialEq)]
pub struct State(Vec<(&'static str, Value)>);

impl State {
    /// The field `key`, when the state has one.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find_map(|(name, value)| (*name == key).then_some(value))
    }

    /// The field names, in wire order.
    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.0.iter().map(|(name, _)| *name)
    }
}

/// `state["move"]` reads like a `Value` index: a field the state lacks reads as null.
impl Index<&str> for State {
    type Output = Value;

    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&Value::Null)
    }
}

impl Serialize for State {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// The state for one assessment. `circumstances` supplies the tool's brief, a bound
/// leaf's approval chain, the promotion probe and the conversation; the supervision
/// facts are the prose rung's alone.
pub(super) fn state(
    role: &Config,
    call: &ToolCall,
    circumstances: &Circumstances<'_>,
    dirs: Option<&Directories>,
    scratch: Option<&std::path::Path>,
) -> State {
    let mut fields: Vec<(&'static str, Value)> = vec![
        (
            "mission",
            json!(
                role.scope
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            ),
        ),
        ("spotter_notes", json!(spotter_notes(role))),
        (
            "working_directory",
            json!(dirs.map(|d| d.judged_working().display().to_string())),
        ),
        ("path_grants", Value::Array(grants(role, dirs))),
        (
            "scratch_directory",
            json!(scratch.map(|p| p.display().to_string())),
        ),
    ];
    if let Some(dirs) = dirs {
        fields.push(("worktrees", json!(super::super::worktree_context(dirs))));
    }
    if let Some(holder) = circumstances.chain.last() {
        // The leaf of an approval chain (docs/permissions.md): the move is the
        // child's, and the chain names who approved on the way here. Present only
        // when there is one, so an ordinary move's state carries no empty field
        // inviting an answer about it.
        fields.push((
            "approval_chain",
            json!({
                "sessions": circumstances.chain,
                "held_by": holder,
            }),
        ));
    }
    fields.push(("guidance", json!(super::super::PERSONA)));
    // Left out, rather than empty, when no session log is at hand: an empty list
    // reads as "nothing was said" (docs/permissions.md).
    if let Some(utterances) = circumstances.utterances {
        fields.push(("conversation", conversation(utterances)));
    }
    fields.push((
        "move",
        proposed_move(role, call, circumstances.brief.as_ref(), dirs, scratch),
    ));
    if let Some(prefix) = circumstances.probe {
        fields.push(("promotion_probe", json!(prefix)));
    }
    State(fields)
}

/// The utterances as `[{writer, text}]`, oldest first, each text whole. The writer is
/// `operator`, `supervisor:<id>` for a supervising agent, or `agent`; the battery's
/// questions say which of them counts as asking.
pub fn conversation(utterances: &Utterances) -> Value {
    utterances
        .said
        .iter()
        .map(|utterance| {
            let writer = match &utterance.speaker {
                Speaker::Operator => "operator".to_string(),
                Speaker::Supervisor(id) => format!("supervisor:{id}"),
                Speaker::Agent => "agent".to_string(),
            };
            json!({ "writer": writer, "text": utterance.text })
        })
        .collect()
}

/// Whether the harness's resolved path facts place `call` outside the grants
/// (docs/permissions.md): one of its paths is under no grant or under a denied one.
/// The fold reads it beside the answers; it is not a question the model is asked.
///
/// A shell command is judged by the facts its state's `move.paths` lists: an entry
/// with no grant or a denied one is outside, an entry that expands at run time is
/// outside when it starts in the home directory (`~`, `$HOME`), and a pattern that may
/// span grants decides nothing. A structured call is judged by the paths in its
/// `path`, `file_path`, `cwd` and `paths` arguments, whatever the tool: one written
/// from the home directory is outside, and the rest resolve against the working
/// directory, with the scratch directory inside and a glob character standing for
/// itself.
pub(super) fn outside_grants(
    role: &Config,
    call: &ToolCall,
    dirs: Option<&Directories>,
    scratch: Option<&std::path::Path>,
) -> bool {
    use crate::config::policy::{Decided, decide, path_words};
    if call.name == BASH {
        let (Some(dirs), Some(command)) = (dirs, call.input.get("command").and_then(Value::as_str))
        else {
            return false;
        };
        let command = judged_command(command, dirs);
        return path_words(command, dirs, scratch).words.iter().any(|word| {
            match decide(role, word, dirs, scratch) {
                Decided::Unresolved => from_home(&word.word),
                Decided::Straddles(_) => false,
                Decided::NoGrant => true,
                Decided::Grant(_, perms) => denied(perms),
            }
        });
    }
    let input = &call.input;
    let mut paths: Vec<&str> = ["path", "file_path", "cwd"]
        .iter()
        .filter_map(|key| input.get(*key).and_then(Value::as_str))
        .collect();
    if let Some(Value::Array(items)) = input.get("paths") {
        paths.extend(items.iter().filter_map(|item| match item {
            Value::Array(parts) => parts.first().and_then(Value::as_str),
            item => item.as_str(),
        }));
    }
    paths.into_iter().any(|path| {
        if from_home(path) {
            return true;
        }
        let Some(dirs) = dirs else {
            return false;
        };
        let target = dirs.target(&path.replace('*', "x"));
        if scratch.is_some_and(|scratch| target.starts_with(scratch)) {
            return false;
        }
        crate::config::policy::governing_grant(role, &target, dirs)
            .is_none_or(|(_, perms)| denied(perms))
    })
}

/// Whether a path as written starts in the home directory.
fn from_home(written: &str) -> bool {
    written.starts_with('~') || written.starts_with("$HOME")
}

fn denied(perms: &crate::config::Perms) -> bool {
    !perms.read && !perms.write
}

/// The shell command the state's move carries: a leading `cd` into the working
/// directory goes nowhere, so the move is the rest (`policy::without_cd_into`) — the
/// resolution the model would otherwise have to do against `working_directory` itself.
fn judged_command<'a>(command: &'a str, dirs: &Directories) -> &'a str {
    crate::config::policy::without_cd_into(command, dirs).unwrap_or(command)
}

/// The path grants, resolved absolute by the same lexical rule the deterministic
/// rung applies (`src/config/policy/paths.rs`), so containment against
/// `working_directory` is string-visible and the model is never asked to resolve
/// the grant side. The configured spelling rides along where it differs, so a move
/// that types `../x` literally still matches something in the list.
///
/// The denies are included: a more specific grant overrides a broader one, and a
/// denied subtree under an allowed parent is exactly the carve-out a bash move can
/// stray into without the deterministic fast-path ever seeing it.
fn grants(role: &Config, dirs: Option<&Directories>) -> Vec<Value> {
    role.paths
        .iter()
        .map(|(path, perms)| {
            let access = access(perms);
            let resolved = dirs.map(|dirs| dirs.grant(path).display().to_string());
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

fn spotter_notes(role: &Config) -> Vec<&str> {
    role.spotter_notes
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
    dirs: &Directories,
    scratch: Option<&std::path::Path>,
) -> (Vec<Value>, bool) {
    let found = crate::config::policy::path_words(command, dirs, scratch);
    let entries = found
        .words
        .into_iter()
        .map(|word| {
            use crate::config::policy::Decided;
            let mut entry = match (&word.resolved, crate::config::policy::decide(role, &word, dirs, scratch)) {
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
                        "resolved": dirs.judged(resolved).display().to_string(),
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
    dirs: Option<&Directories>,
    scratch: Option<&std::path::Path>,
) -> Value {
    if call.name == BASH {
        return match call.input.get("command").and_then(Value::as_str) {
            Some(command) => {
                // The `cd`-less command's paths are the rest's too, so the dropped `cd`
                // target is not listed.
                let command = dirs.map_or(command, |dirs| judged_command(command, dirs));
                let mut shell = json!({
                    "kind": "shell_command",
                    "command": command,
                });
                if let Some((paths, complete)) =
                    dirs.map(|dirs| paths(command, role, dirs, scratch))
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
        object.remove(INTENT);
    }
    let mut proposed = json!({
        "kind": "tool_call",
        "tool": display_name(&call.name),
        "input": input,
    });
    let object = proposed.as_object_mut().expect("built as an object");
    if let Some(brief) = brief {
        let definition = brief.definition;
        object.insert(
            "tool_source".into(),
            match brief.server {
                Some(server) => json!(format!(
                    "the MCP server `{server}`, which this session's configuration connects"
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
            object.insert("tool_description".into(), json!(description));
        }
        let guarantees = super::super::brief::guarantees(definition);
        if !guarantees.is_empty() {
            // Only the safe-direction hints, which hold for every input; the
            // unsafe-direction ones are uninformative about this call (docs/permissions.md,
            // the spotter's brief). `self_reported` records whose guarantee it is.
            object.insert(
                "tool_declarations".into(),
                json!({
                    "self_reported": brief.server.is_some(),
                    "claims": guarantees,
                }),
            );
        }
        object.insert(
            "tool_input_schema".into(),
            json!(
                serde_json::to_string(&definition.input_schema)
                    .unwrap_or_else(|_| definition.input_schema.to_string())
            ),
        );
    }
    proposed
}
