//! The on-disk TOML shape and its validation into a [`Config`] (docs/permissions.md).

use super::*;

/// The literal TOML shape, before validation. `deny_unknown_fields` makes a typo'd key fail
/// loudly — the schema is the contract. [`validate`] folds this into a [`Config`].
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RawConfig {
    #[serde(default)]
    scope: Option<String>,
    #[serde(rename = "watch-for", default)]
    watch_for: Vec<String>,
    #[serde(rename = "trust-threshold", default)]
    trust_threshold: Option<f64>,
    /// `[commands]` table: each value is a [`CommandStance`] or the `bool` sugar
    /// ([`RawCommandValue`]).
    #[serde(default)]
    commands: BTreeMap<String, RawCommandValue>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    paths: BTreeMap<String, RawPathValue>,
    #[serde(default)]
    extends: Vec<String>,
    /// `[context]` table. An [`IndexMap`] (not `BTreeMap`) so **document order is preserved** — the
    /// entries render as preamble sections in the order the operator wrote them. Each value is the
    /// `true` sugar or an inline table ([`RawContextValue`]); [`validate`] interprets each into a
    /// [`ContextEntry`].
    #[serde(default)]
    context: IndexMap<String, RawContextValue>,
    /// `[mcp]` table — connected-provider declarations ([`Config::mcp_servers`]).
    #[serde(default)]
    mcp: RawMcp,
    /// `[bash]` table — the foreground-lifetime knobs ([`Config::bash`]).
    #[serde(default)]
    bash: RawBash,
    /// `[transport]` table — the upstream-stream knobs ([`Config::transport`]).
    #[serde(default)]
    transport: RawTransport,
    /// `[env]` table — per-variable `strip`/`mask` (docs/env.md). Only the inline-table
    /// form parses: a bare `"*_KEY" = false` cannot say which of the two questions it
    /// answers, so it is a parse error rather than a guess. `deny_unknown_fields` on
    /// [`crate::env_table::EnvEntry`] makes a typo'd key loud, as everywhere else.
    #[serde(default)]
    env: BTreeMap<String, crate::env_table::EnvEntry>,
}

/// The `[bash]` table: how long a foreground command may block the turn before
/// it is moved to the background (docs/supervisor.md). Two numbers, because one
/// value cannot be both the default and the clamp — `background_after` is what a
/// call gets when it names nothing, and `background_after_max` is the ceiling a
/// call's own `background_after` is clamped to. The ceiling is the loop-safety
/// property, which is why it is operator-set and not call-set.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RawBash {
    #[serde(rename = "background-after", default)]
    background_after: Option<u64>,
    #[serde(rename = "background-after-max", default)]
    background_after_max: Option<u64>,
}

/// The `[transport]` table: how long one response's SSE stream may stay open
/// before the read loop stops waiting on it (docs/transport.md). Seconds, like
/// `[bash]`'s knobs. Absent → [`crate::api::DEFAULT_STREAM_CEILING`].
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RawTransport {
    #[serde(rename = "stream-ceiling", default)]
    stream_ceiling: Option<u64>,
}

/// The `[mcp]` table. Only `servers` today; a table (rather than hoisting
/// `servers` to the top level) so future mcp-wide knobs have a home.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RawMcp {
    #[serde(default)]
    servers: BTreeMap<String, RawMcpServer>,
}

/// One `[mcp.servers.<name>]` stanza. Which keys are present picks the
/// transport — see [`McpTransport`]; [`validate_mcp_server`] rejects ambiguous
/// or empty shapes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawMcpServer {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    url: Option<String>,
    /// Start the session even if this server does not mount. Defaults to
    /// `false` — see [`McpServerEntry::optional`].
    #[serde(default)]
    optional: bool,
}

/// A `[context]` value: the `true`/`false` sugar (a bare read-only path) or an inline table
/// ([`RawContextEntry`]). Interpreted by [`validate_context_entry`].
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RawContextValue {
    /// `"./CLAUDE.md" = true` → a bare read-only path (the key is the path).
    Sugar(bool),
    /// A table: a labeled group (`{ path|paths = … }`), a dynamic section (`{ exec = … }`), or a
    /// bare path carrying `update` (the key is the path, no `path`/`paths`).
    Table(RawContextEntry),
}

/// The inline-table form of a `[context]` value. Which shape it is (labeled group / exec / bare
/// updatable path) is disambiguated by which keys are present — see [`validate_context_entry`].
#[derive(Deserialize)]
pub(super) struct RawContextEntry {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    exec: Option<String>,
    #[serde(default)]
    update: bool,
    #[serde(default)]
    create: bool,
    #[serde(default)]
    sort: Option<String>,
    /// Paths pruned from a directory's expansion (`exclude = ["MEMORY.md", "archive"]`),
    /// relative to each configured directory. Group-only; a bare path carrying `exclude`
    /// is rejected in [`validate_context_entry`].
    #[serde(default)]
    exclude: Vec<String>,
    /// A prose hint rendered inside the group's tag — what the model should know
    /// about how to use these files (e.g. how to read or write a memory). Group-only;
    /// a bare path carrying `description` is rejected in [`validate_context_entry`].
    #[serde(default)]
    description: Option<String>,
}

/// A `[commands]` value: the stance word (`"clear"` / `"watch"` / `"gate"` / `"deny"`) or the
/// `bool` sugar. Booleans keep parsing because every config written before the vocabulary
/// existed spells its grants that way; `true` = [`CommandStance::Clear`] is what those files
/// already meant, and `false` = [`CommandStance::Deny`] is the carve-out spelling
/// (`git = true` + `"git push" = false`) the longest-prefix resolution reads
/// (docs/permissions.md).
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RawCommandValue {
    Sugar(bool),
    Stance(CommandStance),
}

impl RawCommandValue {
    fn into_stance(self) -> CommandStance {
        match self {
            RawCommandValue::Sugar(true) => CommandStance::Clear,
            RawCommandValue::Sugar(false) => CommandStance::Deny,
            RawCommandValue::Stance(stance) => stance,
        }
    }
}

/// A `[paths]` value: either the `true`/`false` sugar (`true` = read+write) or an explicit
/// `{ read = …, write = … }` inline table.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RawPathValue {
    /// `"." = true` → read+write; `false` → no access.
    Sugar(bool),
    /// `"/refs" = { read = true }` — omitted perms default `false`.
    Perms {
        #[serde(default)]
        read: bool,
        #[serde(default)]
        write: bool,
    },
}

impl RawPathValue {
    fn into_perms(self) -> Perms {
        match self {
            RawPathValue::Sugar(true) => Perms {
                read: true,
                write: true,
            },
            RawPathValue::Sugar(false) => Perms {
                read: false,
                write: false,
            },
            RawPathValue::Perms { read, write } => Perms { read, write },
        }
    }
}

/// A failed validation of an otherwise-parseable TOML file — semantically wrong, not
/// syntactically. Carried by [`LoadError::Validate`].
#[derive(Debug)]
pub enum ValidationError {
    /// A `[commands]` key that does not lex to a single simple command (it has operators,
    /// redirections, expansions, or an env-assignment prefix). The fast-path can only soundly
    /// prefix-match simple commands, so an un-lexable key fails the whole file — no silent skip.
    CommandNotSimple(String),
    /// A `[context]` entry set to `false`. There is nothing to disable — an unwanted entry is
    /// simply not listed — so a `false` is rejected rather than silently doing nothing.
    ContextDisabled(String),
    /// A `[context]` `exec` entry that also carries `path`/`paths`/grants/`sort` — the source is
    /// ambiguous (command stdout *or* files?). An `exec` section has no files to grant or sort.
    ContextExecConflict(String),
    /// A group-only key (`create` or `sort`, named in the payload) on a **bare path** (a `[context]`
    /// entry whose key *is* the path, with no `path`/`paths`). Both are about a *location of many
    /// files*; a single file has nowhere to create into and nothing to sort.
    ContextBarePathGrant(String, &'static str),
    /// An `[mcp.servers.<name>]` stanza with both `command` and `url`, or neither — the transport
    /// is picked by which one is present, so the shape must commit to exactly one.
    McpServerTransport(String),
    /// `args`/`env` on a `url` server — those keys parameterize a spawned subprocess, and an HTTP
    /// server is not spawned.
    McpServerHttpExtras(String),
    /// A `tools` entry whose `*` is not the whole-namespace glob. The only glob the grant set
    /// understands is a terminal `.*` naming a namespace (`ferritin.*`, `efference.*`); a star
    /// anywhere else would read as a pattern the matcher does not implement, so it fails the
    /// whole file rather than silently matching nothing.
    ToolGlob(String),
    /// A server name that cannot prefix a wire tool name. Connected tools reach the wire as
    /// `<server>__<tool>`, and the API constrains tool names to `[a-zA-Z0-9_-]`, so the server
    /// name must fit the same alphabet (and stay short enough to leave room for the tool).
    McpServerName(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::CommandNotSimple(key) => write!(
                f,
                "command key `{key}` is not a single simple command (no operators, \
                 redirections, expansions, or env-assignments) — the fast-path can only \
                 prefix-match simple commands"
            ),
            ValidationError::ContextDisabled(key) => write!(
                f,
                "context entry `{key}` is set to false; there is nothing to disable — remove the \
                 entry instead of listing it as off"
            ),
            ValidationError::ContextExecConflict(key) => write!(
                f,
                "context entry `{key}` mixes `exec` with `path`/`paths`/grants/`sort` — an exec \
                 section's body is the command's stdout and has no files to grant or sort"
            ),
            ValidationError::ContextBarePathGrant(key, which) => write!(
                f,
                "context entry `{key}` uses `{which}` on a bare path — `{which}` needs a labeled \
                 group with `path`/`paths` (a single file has nowhere to create into / nothing to \
                 sort)"
            ),
            ValidationError::McpServerTransport(name) => write!(
                f,
                "mcp server `{name}` must set exactly one of `command` (a stdio subprocess) or \
                 `url` (a streamable-HTTP endpoint)"
            ),
            ValidationError::McpServerHttpExtras(name) => write!(
                f,
                "mcp server `{name}` sets `url` together with `args`/`env` — those keys \
                 parameterize a spawned `command`, and an HTTP server is not spawned"
            ),
            ValidationError::ToolGlob(entry) => write!(
                f,
                "tool grant `{entry}` uses `*` outside the terminal `.*` position — the only \
                 glob is a whole-namespace grant (`ferritin.*` for a server's tools, \
                 `efference.*` for the built-ins); a partial or mid-name star matches nothing"
            ),
            ValidationError::McpServerName(name) => write!(
                f,
                "mcp server name `{name}` must be 1-32 characters of [a-zA-Z0-9_-] and must not \
                 contain `__` (it prefixes every tool the server advertises, as `{name}__<tool>`: \
                 the API constrains tool names to that alphabet, and `__` is the separator itself \
                 — a name containing one would move the namespace boundary)"
            ),
        }
    }
}

/// Parse TOML text into a [`Config`] (decode → validate). The `path` labels diagnostics only.
pub(super) fn parse(path: &Path, text: &str) -> Result<Config, LoadError> {
    let raw: RawConfig =
        toml_edit::de::from_str(text).map_err(|e| LoadError::Parse(path.to_path_buf(), e))?;
    validate(raw).map_err(|e| LoadError::Validate(path.to_path_buf(), e))
}

/// Fold a [`RawConfig`] into the materialized [`Config`], failing closed on any unsound entry.
/// The `[commands]` soundness gate lives here: every key must lex to a simple command (reusing
/// the fast-path's own lexer so the key and the checked command agree token-for-token). A
/// multi-token key is exactly what a carve-out needs (`"git push"` lexes to `["git", "push"]`),
/// so the same check that admits a sub-verb grant admits a sub-verb deny.
pub(super) fn validate(raw: RawConfig) -> Result<Config, ValidationError> {
    let mut commands = BTreeMap::new();
    for (key, raw_value) in raw.commands {
        if crate::policy::simple_command_tokens(&key).is_none() {
            return Err(ValidationError::CommandNotSimple(key));
        }
        commands.insert(key, raw_value.into_stance());
    }
    let paths = raw
        .paths
        .into_iter()
        .map(|(k, v)| (k, v.into_perms()))
        .collect();
    let mut context = Vec::with_capacity(raw.context.len());
    for (key, value) in raw.context {
        context.push(validate_context_entry(key, value)?);
    }
    let mut tools = BTreeSet::new();
    for entry in raw.tools {
        let entry = wire_tool_name(entry);
        if entry.contains('*') && !crate::mcp::is_namespace_glob(&entry) {
            return Err(ValidationError::ToolGlob(crate::mcp::display_name(&entry)));
        }
        tools.insert(entry);
    }
    let mut mcp_servers = BTreeMap::new();
    for (name, server) in raw.mcp.servers {
        let entry = validate_mcp_server(&name, server)?;
        mcp_servers.insert(name, entry);
    }
    Ok(Config {
        scope: raw.scope,
        watch_for: raw.watch_for,
        trust_threshold: raw.trust_threshold,
        commands,
        tools,
        paths,
        extends: raw.extends,
        context,
        mcp_servers,
        env: raw.env,
        bash: crate::config::BashLimits {
            background_after: raw
                .bash
                .background_after
                .map_or(crate::bash::DEFAULT_BACKGROUND_AFTER, Duration::from_secs),
            background_after_max: raw.bash.background_after_max.map_or(
                crate::bash::DEFAULT_BACKGROUND_AFTER_MAX,
                Duration::from_secs,
            ),
        },
        transport: crate::config::TransportLimits {
            stream_ceiling: raw
                .transport
                .stream_ceiling
                .map_or(crate::api::DEFAULT_STREAM_CEILING, Duration::from_secs),
        },
    })
}

/// Normalize a `tools` entry to the name the wire actually carries: the TUI shows a
/// connected tool as `ferritin.get`, so that is what an operator types, but the grant is
/// matched against `ferritin__get`. Only the **first** `.` is the namespace boundary —
/// server names cannot contain one (validated above) while a server's own tool name may
/// (the MCP spec permits `.` and offers `admin.tools.list` as an example), so
/// `ferritin.admin.tools.list` resolves to `ferritin__admin.tools.list`.
///
/// The dotted form is an **input alias only** — it is resolved here and never stored, so
/// the config, the log, and the fast-path all still key on one name. An entry with no `.`
/// (every embedded tool name) passes through untouched.
///
/// A whole-namespace glob takes the same route with no special case: `ferritin.*` →
/// `ferritin__*`, and `efference.*` → `efference__*` — the built-ins' namespace
/// ([`crate::mcp::BUILTIN_NAMESPACE`]) spelled the way every other namespace is, even
/// though no built-in's wire name carries the prefix. [`validate`] then rejects any other
/// use of `*`, and the fast-path resolves the glob against the call's namespace.
pub fn wire_tool_name(entry: String) -> String {
    match entry.split_once('.') {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => {
            format!("{server}{}{tool}", crate::mcp::NAMESPACE_SEPARATOR)
        }
        _ => entry,
    }
}

/// Interpret one `[mcp.servers.<name>]` stanza into an [`McpServerEntry`], failing closed on an
/// ambiguous shape. The name is validated here too, because it prefixes every advertised tool
/// name on the wire (`<server>__<tool>`) and must fit the API's tool-name alphabet.
pub(super) fn validate_mcp_server(
    name: &str,
    server: RawMcpServer,
) -> Result<McpServerEntry, ValidationError> {
    // `__` is excluded, not just absent-by-convention: it is the namespace separator, and
    // every direction of the mapping (wire → display, dotted config → wire) resolves the
    // boundary as the *first* `__`. A server named `my__server` would make that first
    // occurrence land inside the server name, so the name must not contain one.
    let name_ok = !name.is_empty()
        && name.len() <= 32
        && !name.contains(crate::mcp::NAMESPACE_SEPARATOR)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !name_ok {
        return Err(ValidationError::McpServerName(name.to_string()));
    }
    let optional = server.optional;
    let transport = match (server.command, server.url) {
        (Some(command), None) => McpTransport::Stdio {
            command,
            args: server.args,
            env: server.env,
        },
        (None, Some(url)) => {
            if !server.args.is_empty() || !server.env.is_empty() {
                return Err(ValidationError::McpServerHttpExtras(name.to_string()));
            }
            McpTransport::Http { url }
        }
        _ => return Err(ValidationError::McpServerTransport(name.to_string())),
    };
    Ok(McpServerEntry {
        transport,
        optional,
    })
}

/// Interpret one `[context]` `(key, value)` into a [`ContextEntry`], failing closed on an illegal
/// shape (docs/context.md). The value's shape decides the kind:
///
/// - `true` → a bare read-only path (the key is the path, no heading).
/// - a table with `exec` → a labeled dynamic section (no files/grants/sort allowed alongside).
/// - a table with `path`/`paths` → a labeled group (may carry `update`/`create`/`sort`).
/// - a table with neither → a bare path (the key is the path) that may carry only `update`;
///   `create`/`sort` are group-only and rejected here.
pub(super) fn validate_context_entry(
    key: String,
    value: RawContextValue,
) -> Result<ContextEntry, ValidationError> {
    let entry = match value {
        RawContextValue::Sugar(true) => ContextEntry {
            heading: None,
            source: ContextSource::Files(vec![key]),
            grants: ContextGrants::default(),
            sort: None,
            exclude: Vec::new(),
            description: None,
        },
        RawContextValue::Sugar(false) => return Err(ValidationError::ContextDisabled(key)),
        RawContextValue::Table(t) => {
            let has_files = t.path.is_some() || !t.paths.is_empty();
            if let Some(exec) = t.exec {
                // A dynamic section is a labeled group whose body is command stdout: it has no
                // files, so file grants and a sort would be meaningless — reject the mix.
                if has_files || t.update || t.create || t.sort.is_some() || !t.exclude.is_empty() {
                    return Err(ValidationError::ContextExecConflict(key));
                }
                ContextEntry {
                    heading: Some(key),
                    source: ContextSource::Exec(exec),
                    grants: ContextGrants::default(),
                    sort: None,
                    exclude: Vec::new(),
                    description: t.description,
                }
            } else if has_files {
                // Labeled group: the key is a heading; the location is `path` then any `paths`.
                let mut files = Vec::new();
                files.extend(t.path);
                files.extend(t.paths);
                ContextEntry {
                    heading: Some(key),
                    source: ContextSource::Files(files),
                    grants: ContextGrants {
                        update: t.update,
                        create: t.create,
                    },
                    sort: t.sort,
                    exclude: t.exclude,
                    description: t.description,
                }
            } else {
                // Bare path carrying grants: the key itself is the path, no heading. Only `update`
                // is legal here — `create`/`sort` are about a location of many files.
                if t.create {
                    return Err(ValidationError::ContextBarePathGrant(key, "create"));
                }
                if t.sort.is_some() {
                    return Err(ValidationError::ContextBarePathGrant(key, "sort"));
                }
                if t.description.is_some() {
                    return Err(ValidationError::ContextBarePathGrant(key, "description"));
                }
                if !t.exclude.is_empty() {
                    return Err(ValidationError::ContextBarePathGrant(key, "exclude"));
                }
                ContextEntry {
                    heading: None,
                    source: ContextSource::Files(vec![key]),
                    grants: ContextGrants {
                        update: t.update,
                        create: false,
                    },
                    sort: None,
                    exclude: Vec::new(),
                    description: None,
                }
            }
        }
    };
    Ok(entry)
}
