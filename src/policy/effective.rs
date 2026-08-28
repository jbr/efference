//! Folding logged config edits into the effective policy (docs/permissions.md).

use super::*;

/// Fold the log's [`Payload::ConfigEdit`] changes onto a base config, yielding the **effective
/// config** the classifier should consult (docs/permissions.md). The effective
/// policy at any point is `base + fold(changes)` — the same fold-over-the-log shape as the wire: this
/// is materializing a table from an ordered log, exactly as a database replays its write-ahead
/// log (kin to `project(log)`, docs/log-and-projection.md). The base config is the table's initial state;
/// each change is a `set key = value` the replay applies in order.
///
/// Each change is **replace-at-key** (last-write-wins). The changes the system emits happen to
/// be additive (membership in a set, a perm on a path), so the fold is also idempotent —
/// replaying a change already persisted to the file *and* still in the log lands the same
/// value. Order matters in general, though: the table is materialized by replaying in
/// sequence. This batch form rebuilds from the whole log (used at
/// startup to fold a resumed log, and in tests); the daemon then maintains its effective config
/// **incrementally** via [`apply_config_change`], so a gate never re-scans the log.
pub fn effective_config(base: &Config, log: &[LogEntry]) -> Config {
    let mut role = base.clone();
    for entry in log {
        // Only a committed edit takes effect; a `Proposed`/`Cancelled` entry is audit-only and
        // inert in the effective config (docs/permissions.md).
        if let Payload::ConfigEdit(edit) = &entry.payload
            && edit.state == crate::log::EditState::Committed
        {
            apply_config_change(&mut role, &edit.change);
        }
    }
    role
}

/// Apply one [`ConfigChange`] to a config in place — the per-change step of [`effective_config`],
/// exposed so the daemon can update its effective config *incrementally* (applying each change as
/// it appends) rather than re-scanning the whole log on every gate (docs/control-channel.md).
/// Replace-at-key: each variant sets the addressed key in the matching set/map.
pub fn apply_config_change(role: &mut Config, change: &ConfigChange) {
    match change {
        // A `Command` change is a **clear** grant: the change vocabulary spells the grant an
        // approval hands out (a human's approve-always, a promoted prefix), and there is no
        // initiator that hands out a restriction. A non-clear stance is written in the file and
        // read at load (docs/permissions.md).
        ConfigChange::Command { command } => {
            role.commands
                .insert(command.clone(), crate::config::CommandStance::Clear);
        }
        ConfigChange::Path { path, read, write } => {
            role.paths.insert(
                path.clone(),
                Perms {
                    read: *read,
                    write: *write,
                },
            );
        }
        ConfigChange::Tool { name } => {
            role.tools.insert(name.clone());
        }
        ConfigChange::Scope { text } => {
            role.scope = Some(text.clone());
        }
    }
}
