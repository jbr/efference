//! The grant a gate offers: one config change that would clear the held call, or none
//! (docs/permissions.md).

use super::*;

/// The one grant a gate offers alongside approve and abort (docs/permissions.md): the
/// change a client commits unchanged as the `config_edit` decision, and the label that
/// names what it allows ("always allow reading /work/refs"). Surfaced on `/state`'s
/// `gate` topic (docs/control-api.md); never logged until committed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GateGrant {
    pub change: ConfigChange,
    pub label: String,
}

/// The grant a gate holding `call` offers, or `None` when no single change is
/// confidently the one (docs/permissions.md). The caller asks only where the cascade
/// held the call on a `Gate` verdict: a `[commands]` deny, a config write, a hand-off,
/// and a bound `agents__decide` offer nothing.
///
/// - A **structured** call missing only its tool grant offers that tool; one missing only
///   path grants offers a `[paths]` grant for the directory the call reached, read-only
///   when the call only reads. Missing both is two reasons, and offers nothing.
/// - A **bash** call offers a `[commands]` prefix: the shortest token-prefix of the bare
///   command that no spotter probe has voted not safe ([`ProbeLedger::refuted`]) — the
///   program name when the ledger holds no evidence. A command stage 1 rejects (a pipe, a
///   `cd` into an ungranted directory) offers nothing: no prefix would clear it.
///
/// Every offer is checked by applying it to `role` and re-classifying the call: a change
/// that would leave the call held is not offered, since committing it would only redirect.
pub fn gate_grant(
    dirs: &Directories,
    role: &Config,
    ledger: &ProbeLedger,
    call: &ToolCall,
) -> Option<GateGrant> {
    let clears = |change: &ConfigChange| {
        let mut amended = role.clone();
        apply_config_change(&mut amended, change);
        classify_in(&amended, call, dirs) == Verdict::Clear
    };
    let change = if call.name == bash::NAME {
        let command = call.input.get("command").and_then(Value::as_str)?;
        let tokens = bash_tokens_in(role, command, dirs)?;
        if commands::command_stance(role, &tokens) == Some(CommandStance::Deny) {
            return None;
        }
        (1..=tokens.len())
            .map(|len| (len, tokens[..len].join(" ")))
            // The key must re-lex to the very tokens it was cut from, or it would
            // match some other command than this one.
            .filter(|(len, prefix)| {
                simple_command_tokens(prefix).as_deref() == Some(&tokens[..*len])
            })
            .filter(|(_, prefix)| !ledger.refuted(prefix))
            .map(|(_, command)| ConfigChange::Command { command })
            .find(&clears)?
    } else {
        let missing = missing_grants_in(role, call, dirs)?;
        let change = match missing.tool {
            Some(name) if missing.paths.is_empty() => ConfigChange::Tool { name },
            None => path_grant(role, dirs, &missing.paths, missing.needs_write)?,
            Some(_) => return None,
        };
        clears(&change).then_some(change)?
    };
    Some(GateGrant {
        label: label(&change),
        change,
    })
}

/// A `[paths]` grant covering every one of `targets` at the directory they reached: the
/// shallowest such directory, when it contains the rest. `None` when they diverge, or when
/// a target sits under a no-access grant — a deny is the operator's (or the self-surface's)
/// answer, which an offer does not reverse.
fn path_grant(
    role: &Config,
    dirs: &Directories,
    targets: &[String],
    write: bool,
) -> Option<ConfigChange> {
    let mut reached = Vec::new();
    for target in targets {
        let judged = dirs.target(target);
        if governing_grant(role, &judged, dirs).is_some_and(|(_, p)| !p.read && !p.write) {
            return None;
        }
        // The one filesystem touch: whether the call named a directory (a `list`, a
        // `search`) or a file in one. Harmless here, as in [`context_granted`]: the offer
        // is control state, never on the wire.
        let real = lexical_normalize(&absolutize(target, dirs.working()));
        reached.push(match real.is_dir() {
            true => judged,
            false => judged.parent().map(Path::to_path_buf).unwrap_or(judged),
        });
    }
    let dir = reached.iter().min_by_key(|d| d.components().count())?;
    if !reached.iter().all(|d| d.starts_with(dir)) {
        return None;
    }
    // A key already naming that directory is replaced rather than joined by a second
    // spelling of it, which would tie with it for the deepest grant.
    let path = match role.paths.keys().find(|key| dirs.grant(key) == *dir) {
        Some(key) => key.clone(),
        None => dir.to_str()?.to_string(),
    };
    Some(ConfigChange::Path {
        path,
        read: true,
        write,
    })
}

/// What the grant allows, in the words a client shows on its control.
fn label(change: &ConfigChange) -> String {
    match change {
        ConfigChange::Command { command } => {
            format!("always allow running {command} with any arguments")
        }
        ConfigChange::Path {
            path, write: true, ..
        } => format!("always allow reading and writing {path}"),
        ConfigChange::Path { path, .. } => format!("always allow reading {path}"),
        ConfigChange::Tool { name } => {
            format!("always allow the tool {}", crate::mcp::display_name(name))
        }
        ConfigChange::Scope { .. } => "replace the session scope".to_string(),
    }
}
