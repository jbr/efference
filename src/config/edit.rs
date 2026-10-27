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
    // Commands: additive membership — emit each key the file clears that effective doesn't
    // already clear. A key carrying a non-clear stance (`watch`/`gate`/`deny`) is **skipped**,
    // because [`ConfigChange::Command`] can only say "clear" and applying it would invert the
    // author's meaning. So a carve-out added by file edit takes effect on the next boot (which
    // reloads the file as the base), exactly like a deleted key — the same honest frontier of
    // the additive fold, reached by a tightening that is written rather than removed.
    for (command, stance) in &to.commands {
        if *stance == crate::config::CommandStance::Clear
            && from.commands.get(command) != Some(stance)
        {
            out.push(ConfigChange::Command {
                command: command.clone(),
            });
        }
    }
    // Tools: additive grants only, like commands. A namespace grant arrives as its glob
    // (`ferritin__*`); `enabled` is read at boot and has no live change to emit.
    for name in to.tools.grant_names().difference(&from.tools.grant_names()) {
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
        // itself (already re-lexable), so there is no token-join to undo. `true` rather than
        // `"clear"` because the sugar is what every hand-written config uses and what the two
        // spellings agree on at load; the round-trip is therefore stance-preserving.
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
        // A tool grant: set `granted = true` on the `[tools]` entry, adding the entry when the
        // file has none. A namespace glob (`ferritin__*`) is the namespace's bare key; a tool is
        // its dotted key (`ferritin.get`, `efference.read`), which toml_edit quotes. The change
        // carries the wire name (the log is a machine record); the key is applied here, at the
        // document boundary, and undone by the `[tools]` validation on the next load, so the
        // reloaded file grants the same name and the idempotent echo diffs to nothing.
        ConfigChange::Tool { name } => {
            let key = match crate::mcp::is_namespace_glob(name) {
                true => name
                    .strip_suffix(&crate::mcp::namespace_glob(""))
                    .unwrap_or(name)
                    .to_string(),
                false => tools::table_key(name),
            };
            let table = doc
                .as_table_mut()
                .entry("tools")
                .or_insert(Item::Table(Table::new()));
            match table.get_mut(&key) {
                Some(Item::Table(entry)) => entry["granted"] = value(true),
                Some(Item::Value(Value::InlineTable(entry))) => {
                    entry.insert("granted", Value::from(true));
                    // `insert` copies the neighbouring key's spacing; normalize the braces.
                    entry.fmt();
                }
                _ => {
                    let mut entry = InlineTable::new();
                    entry.insert("granted", Value::from(true));
                    table[key.as_str()] = value(entry);
                }
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
