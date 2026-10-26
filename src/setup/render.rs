//! A [`Draft`] as the TOML `src/inference.rs` reads (docs/operating.md).
//!
//! Rendering goes through a format-preserving document so an existing file keeps
//! its comments and order: new `[connections.<name>]` tables are appended, and
//! `[defaults]` is set only when the draft chose one. A fresh file gets a short
//! header saying what the file is and what it never holds, under the `#:schema`
//! line that gives an editor its published schema (docs/config-schemas.md). An
//! existing file is the user's, so it gains no schema line it did not have.

use std::path::Path;

use toml_edit::{DocumentMut, InlineTable, Item, Table, value};

use super::*;

const HEADER: &str = "\
# The efference user configuration, written by `efference setup`.
# Connections name a provider and where its credential comes from; the
# credential itself is never in this file. Sessions select a model as
# <connection>/<native model>; [defaults] names the spotter.
# Reference: docs/operating.md.

";

/// Fold `draft` into `existing_text` (an empty string for no file) and return
/// the new file contents. Errors only if the existing text is not TOML, or an
/// edit names a connection the text does not have.
pub fn render(existing_text: &str, draft: &Draft) -> Result<String, String> {
    let fresh = existing_text.trim().is_empty();
    let mut doc: DocumentMut = existing_text
        .parse()
        .map_err(|error: toml_edit::TomlError| error.to_string())?;
    for change in &draft.edits {
        apply_change(&mut doc, change)?;
    }
    if let Some(defaults) = doc.get("defaults").and_then(|item| item.as_table_like())
        && defaults.is_empty()
    {
        doc.remove("defaults");
    }
    if doc.get("connections").is_none() {
        let mut connections = Table::new();
        connections.set_implicit(true);
        doc.insert("connections", Item::Table(connections));
    }
    let connections = doc["connections"]
        .as_table_mut()
        .ok_or("[connections] is not a table")?;
    for (name, connection) in &draft.connections {
        let mut table = Table::new();
        // A described endpoint is written as what it is — a dialect and a base
        // URL — never as a `provider` line naming a variant that is not a
        // provider name (docs/transport.md).
        match (connection.provider, &connection.base_url) {
            (Provider::Custom(dialect), Some(base)) => {
                table["dialect"] = value(dialect.config_name());
                table["base-url"] = value(base.as_str());
            }
            _ => table["provider"] = value(connection.provider.name()),
        }
        table["auth"] = value(auth_inline_table(&connection.auth));
        if connection.disabled {
            table["disabled"] = value(true);
        }
        connections.insert(name, Item::Table(table));
    }
    let body = crate::config_schemas::keep_directives_first(existing_text, doc.to_string());
    Ok(if fresh {
        let directive = crate::config_schemas::directive(crate::config_schemas::USER);
        format!("{directive}\n{HEADER}{body}")
    } else {
        body
    })
}

/// `auth = { kind = "…" }` as `src/inference.rs` reads it.
fn auth_inline_table(auth: &AuthSource) -> InlineTable {
    let mut table = InlineTable::new();
    match auth {
        AuthSource::Env { key } => {
            table.insert("kind", "env".into());
            table.insert("key", key.as_str().into());
        }
        AuthSource::CodexCache => {
            table.insert("kind", "codex-cache".into());
        }
        AuthSource::Keychain => {
            table.insert("kind", "keychain".into());
        }
        AuthSource::Oauth => {
            table.insert("kind", "oauth".into());
        }
    }
    table
}

/// Apply one management-flow mutation (`src/setup/manage.rs`) to the document
/// in place. Any secret-store side effect a change implies (a rename's item
/// move, a delete's offered item removal) already happened before this runs;
/// this only ever touches the TOML.
fn apply_change(doc: &mut DocumentMut, change: &Change) -> Result<(), String> {
    match change {
        Change::Rename { from, to } => {
            let connections = doc["connections"]
                .as_table_mut()
                .ok_or("[connections] is not a table")?;
            let item = connections
                .remove(from)
                .ok_or_else(|| format!("connection {from:?} is not in the file"))?;
            connections.insert(to, item);
            if let Some(defaults) = doc
                .get_mut("defaults")
                .and_then(|item| item.as_table_like_mut())
            {
                let prefix = format!("{from}/");
                let rewritten: Vec<(&str, String)> = ["spotter"]
                    .into_iter()
                    .filter_map(|key| {
                        let current = defaults.get(key)?.as_str()?;
                        let rest = current.strip_prefix(&prefix)?;
                        Some((key, format!("{to}/{rest}")))
                    })
                    .collect();
                for (key, new_value) in rewritten {
                    defaults.insert(key, value(new_value));
                }
            }
        }
        Change::SetAuth { name, auth } => {
            connection_table(doc, name)?["auth"] = value(auth_inline_table(auth));
        }
        Change::SetDisabled { name, disabled } => {
            let table = connection_table(doc, name)?;
            if *disabled {
                table["disabled"] = value(true);
            } else {
                table.remove("disabled");
            }
        }
        Change::Delete { name } => {
            let connections = doc["connections"]
                .as_table_mut()
                .ok_or("[connections] is not a table")?;
            connections
                .remove(name)
                .ok_or_else(|| format!("connection {name:?} is not in the file"))?;
        }
        Change::SetDefaultRole {
            role,
            value: role_value,
        } => match role_value {
            Some(qualified) => {
                if doc.get("defaults").is_none() {
                    let mut defaults = Table::new();
                    defaults.set_position(Some(0));
                    doc.insert("defaults", Item::Table(defaults));
                }
                doc["defaults"][role.key()] = value(qualified);
            }
            None => {
                if let Some(defaults) = doc
                    .get_mut("defaults")
                    .and_then(|item| item.as_table_like_mut())
                {
                    defaults.remove(role.key());
                }
            }
        },
    }
    Ok(())
}

fn connection_table<'doc>(
    doc: &'doc mut DocumentMut,
    name: &str,
) -> Result<&'doc mut Table, String> {
    doc["connections"]
        .as_table_mut()
        .ok_or("[connections] is not a table")?
        .get_mut(name)
        .and_then(|item| item.as_table_mut())
        .ok_or_else(|| format!("connection {name:?} is not in the file"))
}

/// Write `contents` at `path`, creating the parent directory, with permissions
/// that keep the file to its owner. The file holds no secret, but it names
/// where they are, which is enough to keep private.
pub fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write as _;
    options.open(path)?.write_all(contents.as_bytes())
}
