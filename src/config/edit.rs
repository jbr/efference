//! Diffing two configs into [`ConfigChange`]s and applying one to a TOML document (docs/permissions.md).

use super::*;

/// The additive/replace deltas that carry `from` to `to`, as [`ConfigChange`]s — the diff the
/// fs-watch initiator emits when the config file changes (docs/permissions.md).
/// `from` is the current effective config, `to` the freshly-reloaded file, so only genuinely-new
/// deltas surface (the harness's own auto-persist write-back diffs to nothing — the idempotent echo).
///
/// **Additive only**, by the shape of [`apply_config_change`]: it can add a command/tool, set a
/// path's perms, or replace the scope text — it cannot *remove* a set member or clear scope. So a
/// key deleted from the file is **not** propagated live (the effective config keeps it); tightening
/// by file edit takes effect on the next boot, which reloads the file as the base. This is the
/// honest frontier of the additive fold, not a bug to paper over. `watch-for` is likewise skipped —
/// it has no [`ConfigChange`] variant yet (see [`crate::log::ConfigChange`]). Emission order is
/// deterministic: the `BTreeSet`/`BTreeMap` slots iterate sorted.
pub fn diff_changes(from: &Config, to: &Config) -> Vec<ConfigChange> {
    let mut out = Vec::new();
    // Scope: a scalar replace — emit when the file's scope differs (and is set; `None` can't clear).
    if let Some(text) = &to.scope
        && from.scope.as_ref() != Some(text)
    {
        out.push(ConfigChange::Scope { text: text.clone() });
    }
    // Commands / tools: additive set membership — emit each key the file has that effective lacks.
    for command in to.commands.difference(&from.commands) {
        out.push(ConfigChange::Command {
            command: command.clone(),
        });
    }
    for name in to.tools.difference(&from.tools) {
        out.push(ConfigChange::Tool { name: name.clone() });
    }
    // Paths: a perms map — emit each path whose perms are new or changed in the file.
    for (path, perms) in &to.paths {
        if from.paths.get(path) != Some(perms) {
            out.push(ConfigChange::Path {
                path: path.clone(),
                read: perms.read,
                write: perms.write,
            });
        }
    }
    out
}

/// Edit a committed [`ConfigChange`] into a TOML document in place
/// (docs/permissions.md). Replaces at a key, leaving every comment and unrelated
/// line untouched; see [`ConfigFile::apply`], the only caller, for the surrounding invariant.
pub(super) fn apply_to_document(doc: &mut DocumentMut, change: &ConfigChange) {
    match change {
        // A command-prefix grant: set `[commands][key] = true`. The key is the command string
        // itself (already re-lexable), so there is no token-join to undo.
        ConfigChange::Command { command } => {
            doc["commands"][command.as_str()] = value(true);
        }
        // A filesystem grant: set `[paths][path]` to the sugar or an inline `{ read, write }`.
        ConfigChange::Path {
            path: p,
            read,
            write,
        } => {
            doc["paths"][p.as_str()] = path_value(*read, *write);
        }
        // A tool-name grant: append to the `tools` array if absent (the array is set-shaped, so
        // dedup is membership, not last-write-wins).
        ConfigChange::Tool { name } => {
            let entry = doc
                .as_table_mut()
                .entry("tools")
                .or_insert(Item::Value(Value::Array(Array::new())));
            if let Some(arr) = entry.as_array_mut()
                && !arr.iter().any(|v| v.as_str() == Some(name.as_str()))
            {
                arr.push(name.as_str());
            }
        }
        // A scope replace: set the `scope` scalar in place. When it already exists, carry its
        // decor over so the author's inline comment survives the narrowing (the auto-persist
        // guarantee; cf. docs/permissions.md).
        ConfigChange::Scope { text } => match doc.get_mut("scope").and_then(Item::as_value_mut) {
            Some(existing) => {
                let decor = existing.decor().clone();
                *existing = Value::from(text.as_str());
                *existing.decor_mut() = decor;
            }
            None => doc["scope"] = value(text.as_str()),
        },
    }
}

/// Render a `[paths]` value: the `true` sugar when read+write, otherwise an inline table
/// carrying exactly the granted perms (`{ read = true }`), or `false` when nothing is granted.
pub(super) fn path_value(read: bool, write: bool) -> Item {
    if read && write {
        return value(true);
    }
    if !read && !write {
        return value(false);
    }
    let mut table = InlineTable::new();
    if read {
        table.insert("read", Value::from(true));
    }
    if write {
        table.insert("write", Value::from(true));
    }
    value(table)
}
