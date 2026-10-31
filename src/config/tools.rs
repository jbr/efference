//! The `[tools]` table: which tools a session has, and which of those run without review
//! (docs/permissions.md, docs/tools.md).
//!
//! A bare key names a namespace and a quoted dotted key names one tool; a tool's entry beats
//! its namespace's, field by field. `enabled` is read at boot, when the toolbox is mounted, so
//! the session's `tools[]` never moves mid-session (docs/log-and-projection.md); `granted` is
//! the fast path's name grant and moves live with the session's grants.

use super::*;
use crate::names::tools::{
    AGENTS_NAMESPACE, ASK, BUILTIN_NAMESPACE, CHECK_INBOX, CONSOLIDATE, KILL_PROCESS,
    LIST_PROCESSES, LSP_NAMESPACE, NAMESPACE_SEPARATOR, READ_PROCESS, WAIT, WATCH_PROCESS,
    display_name, is_namespace_glob, is_triage_tool, namespace_glob, namespace_of,
};

/// The namespaces efference serves itself: [`BUILTIN_NAMESPACE`] for the
/// unprefixed built-ins (`read`, `bash`, …), the session-control verbs (`agents__<verb>`),
/// and the language-server tools (`lsp__<tool>`). Off unless a `[tools]` entry enables them,
/// and no declared server may take their names.
pub const BUILTIN_NAMESPACES: [&str; 3] = [BUILTIN_NAMESPACE, AGENTS_NAMESPACE, LSP_NAMESPACE];

/// One `[tools]` entry as written. A field left `None` comes from the namespace's entry, then
/// the default ([`Config::tool_enabled`], [`ToolsTable::granted`]). Serialized as the `role`
/// topic reports an entry, with only the fields it sets (docs/control-api.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ToolSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<bool>,
}

/// The `[tools]` table, keyed for lookup by wire name. Namespace entries and tool entries are
/// held apart because a built-in's wire name is bare, like a namespace: `read` the tool and a
/// namespace called `read` would otherwise share a key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolsTable {
    /// Entries for a whole namespace (`efference`, `agents`, a declared server).
    pub namespaces: BTreeMap<String, ToolSettings>,
    /// Entries for one tool, keyed by its wire name (`read`, `agents__message`).
    pub tools: BTreeMap<String, ToolSettings>,
}

impl ToolsTable {
    /// Whether a call to `wire_name` clears the fast path's name check (docs/permissions.md).
    /// The grant is a statement of trust in a party: a granted namespace covers every tool it
    /// advertises now and any it adds later, so it is only ever written, never a default.
    pub fn granted(&self, wire_name: &str) -> bool {
        self.resolve(wire_name, |s| s.granted).unwrap_or(false)
    }

    /// Whether `wire_name` is mounted, given the default for its namespace.
    fn enabled(&self, wire_name: &str, default: bool) -> bool {
        self.resolve(wire_name, |s| s.enabled).unwrap_or(default)
    }

    fn resolve(
        &self,
        wire_name: &str,
        field: impl Fn(&ToolSettings) -> Option<bool>,
    ) -> Option<bool> {
        self.tools.get(wire_name).and_then(&field).or_else(|| {
            self.namespaces
                .get(namespace_of(wire_name))
                .and_then(&field)
        })
    }

    /// Apply a logged grant ([`ConfigChange::Tool`]): a wire name grants that tool, and a
    /// namespace glob (`ferritin__*`, [`namespace_glob`]) the whole namespace.
    pub fn grant(&mut self, name: &str) {
        match namespace_of_glob(name) {
            Some(namespace) => {
                self.namespaces
                    .entry(namespace.to_string())
                    .or_default()
                    .granted = Some(true);
            }
            None => {
                // A config written before the `[tools]` table spelled a built-in `efference.read`,
                // which it logged as `efference__read`; the built-in's wire name is `read`.
                let builtin = format!("{BUILTIN_NAMESPACE}{NAMESPACE_SEPARATOR}");
                let wire = name.strip_prefix(&builtin).unwrap_or(name);
                self.tools.entry(wire.to_string()).or_default().granted = Some(true);
            }
        }
    }

    /// Every grant the table writes, in the form the log names one ([`ConfigChange::Tool`]):
    /// a namespace as its glob, a tool as its wire name. Sorted, for a deterministic diff.
    pub fn grant_names(&self) -> BTreeSet<String> {
        let namespaces = self
            .namespaces
            .iter()
            .filter(|(_, s)| s.granted == Some(true))
            .map(|(namespace, _)| namespace_glob(namespace));
        let tools = self
            .tools
            .iter()
            .filter(|(_, s)| s.granted == Some(true))
            .map(|(name, _)| name.clone());
        namespaces.chain(tools).collect()
    }

    /// Every entry under the key the file spells it with (docs/control-api.md's `/state`).
    pub fn entries(&self) -> impl Iterator<Item = (String, ToolSettings)> + '_ {
        let namespaces = self.namespaces.iter().map(|(k, s)| (k.clone(), *s));
        let tools = self.tools.iter().map(|(k, s)| (table_key(k), *s));
        namespaces.chain(tools)
    }
}

/// The namespace a logged whole-namespace grant names, or `None` for a tool's wire name.
fn namespace_of_glob(name: &str) -> Option<&str> {
    is_namespace_glob(name)
        .then(|| name.strip_suffix(&namespace_glob("")))
        .flatten()
}

/// The `[tools]` key for a tool's wire name: `efference.read` for the unprefixed built-in
/// `read`, `ferritin.get` for `ferritin__get`. Always dotted, because a bare key is a namespace.
pub fn table_key(wire_name: &str) -> String {
    if wire_name.contains(NAMESPACE_SEPARATOR) {
        display_name(wire_name)
    } else {
        format!("{BUILTIN_NAMESPACE}.{wire_name}")
    }
}

/// The tools the turn loop itself drives (docs/process-monitor.md, docs/consolidate.md):
/// mounted in every session whatever `[tools]` says, because the loop asks for `consolidate`
/// and a backgrounded command is read through the process tools. They take a grant like any
/// other tool, but not `enabled`.
pub fn always_present(wire_name: &str) -> bool {
    [
        CONSOLIDATE,
        LIST_PROCESSES,
        READ_PROCESS,
        KILL_PROCESS,
        WATCH_PROCESS,
        CHECK_INBOX,
        WAIT,
        ASK,
    ]
    .contains(&wire_name)
        || is_triage_tool(wire_name)
}

/// Whether a namespace's tools are mounted when no entry says: a declared server's are (the
/// declaration is the opt-in), efference's own are not.
pub(super) fn enabled_by_default(
    namespace: &str,
    mcp_servers: &BTreeMap<String, McpServerEntry>,
) -> bool {
    !BUILTIN_NAMESPACES.contains(&namespace) && mcp_servers.contains_key(namespace)
}

impl Config {
    /// Whether the session mounts `wire_name` (docs/tools.md): the loop's own tools always,
    /// anything else as `[tools]` resolves it.
    pub fn tool_enabled(&self, wire_name: &str) -> bool {
        always_present(wire_name)
            || self.tools.enabled(
                wire_name,
                enabled_by_default(namespace_of(wire_name), &self.mcp_servers),
            )
    }
}

/// Fold the written `[tools]` table into a [`ToolsTable`], failing closed on a key that names
/// nothing, a glob, `enabled` on a tool the loop always mounts, or a grant on a tool that is
/// not mounted. `mcp_servers` is the validated `[mcp.servers]`, whose names are namespaces.
pub(super) fn validate_tools(
    raw: BTreeMap<String, RawToolSettings>,
    mcp_servers: &BTreeMap<String, McpServerEntry>,
) -> Result<ToolsTable, ValidationError> {
    let known = |namespace: &str| {
        BUILTIN_NAMESPACES.contains(&namespace) || mcp_servers.contains_key(namespace)
    };
    let mut table = ToolsTable::default();
    for (key, settings) in raw {
        let settings = ToolSettings {
            enabled: settings.enabled,
            granted: settings.granted,
        };
        if key.contains('*') {
            return Err(ValidationError::ToolGlob(key));
        }
        match key.split_once('.') {
            None if known(&key) => {
                table.namespaces.insert(key, settings);
            }
            Some((namespace, tool)) if !tool.is_empty() && known(namespace) => {
                let wire = if namespace == BUILTIN_NAMESPACE {
                    tool.to_string()
                } else {
                    format!("{namespace}{NAMESPACE_SEPARATOR}{tool}")
                };
                if settings.enabled.is_some() && always_present(&wire) {
                    return Err(ValidationError::ToolAlwaysPresent(key));
                }
                table.tools.insert(wire, settings);
            }
            _ => return Err(ValidationError::ToolNamespace(key)),
        }
    }
    for (namespace, settings) in &table.namespaces {
        let enabled = settings
            .enabled
            .unwrap_or_else(|| enabled_by_default(namespace, mcp_servers));
        if settings.granted == Some(true) && !enabled {
            return Err(ValidationError::ToolGrantedNotEnabled(namespace.clone()));
        }
    }
    for (wire, settings) in &table.tools {
        let enabled = always_present(wire)
            || table.enabled(wire, enabled_by_default(namespace_of(wire), mcp_servers));
        if settings.granted == Some(true) && !enabled {
            return Err(ValidationError::ToolGrantedNotEnabled(table_key(wire)));
        }
    }
    Ok(table)
}

/// A `[tools]` entry: `enabled` puts the tool in the session's tool list, and `granted` lets a
/// call to it run without review. A key left out comes from the namespace's entry.
// Deserialized by hand so a key other than the two is named back with the likely cause: an
// unquoted dotted key (`agents.message = { … }`) is a TOML nested table, which arrives here as
// a namespace entry carrying a `message` key. `deny_unknown_fields` is for the schema.
#[derive(Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "ToolEntry")]
pub(super) struct RawToolSettings {
    /// Put this tool, or every tool in this namespace, in the session's tool list. efference's
    /// own tools are left out unless enabled; a server declared under `[mcp.servers]` is in.
    /// Read when the session starts.
    enabled: Option<bool>,
    /// Let calls run without review. A tool that takes a path also needs that path granted in
    /// `[paths]`. A granted tool must be enabled.
    granted: Option<bool>,
}

impl<'de> Deserialize<'de> for RawToolSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SettingsVisitor;
        impl<'de> serde::de::Visitor<'de> for SettingsVisitor {
            type Value = RawToolSettings;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "an inline table of `enabled` and `granted`, as in `{ enabled = true }`",
                )
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<RawToolSettings, A::Error> {
                let mut settings = RawToolSettings::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "enabled" => settings.enabled = Some(map.next_value()?),
                        "granted" => settings.granted = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::custom(format!(
                                "unknown key `{other}` in a `[tools]` entry, which takes \
                                 `enabled` and `granted`; if `{other}` is a tool, quote its \
                                 dotted name: `\"<namespace>.{other}\" = {{ … }}`"
                            )));
                        }
                    }
                }
                Ok(settings)
            }
        }
        deserializer.deserialize_map(SettingsVisitor)
    }
}
