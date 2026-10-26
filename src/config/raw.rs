//! The on-disk TOML shape and its validation into a [`Config`] (docs/permissions.md).
//!
//! The types here are also the source of the published project schema
//! (docs/config-schemas.md): their `///` comments are the hover text an editor shows
//! someone writing `efference.toml`, and `//` comments are for implementers.

use super::*;

/// Policy for sessions in this project: what they are for, what they may do without
/// review, and what they start out knowing. Read from `efference.toml`; each session
/// keeps its own copy as `.efference/<session>/config.toml`. Every key is optional, and
/// an unknown key or an entry efference cannot read soundly fails the whole file.
// The literal TOML shape, before validation. `deny_unknown_fields` makes a typo'd key fail
// loudly. [`validate`] folds this into a [`Config`].
#[derive(Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    /// What sessions here are for, in prose. It opens each session's preamble, and the
    /// spotter judges every move it reviews against it: the question is whether the move
    /// is inside this scope, not whether it is dangerous.
    #[serde(default)]
    scope: Option<String>,
    /// Hazards the spotter should look out for in this project. Shown to the spotter
    /// only, never to the session.
    #[serde(rename = "watch-for", default)]
    watch_for: Vec<String>,
    /// Reserved: parsed, with no effect.
    #[serde(rename = "trust-threshold", default)]
    trust_threshold: Option<f64>,
    /// A user turn sent when a session starts with nothing yet said, so the agent speaks
    /// first. Read once at boot and not drawn in the clients. Omit it for sessions that
    /// wait for the operator.
    #[serde(rename = "first-turn", default)]
    first_turn: Option<String>,
    /// How much new tool work, in bytes, prompts the session to consolidate it: tool
    /// arguments plus the result text it kept. Images and dialogue do not count.
    /// Default `65536` (64 KiB).
    #[serde(rename = "consolidation-bytes", default)]
    consolidation_bytes: Option<usize>,
    /// Bash command prefixes and the stance each one gets, as in `cargo = true` or
    /// `"git push" = "deny"`. The longest matching prefix decides, so a broad grant can
    /// carry exceptions. A key that is not a single simple command fails the file.
    /// Grants apply only to structurally simple commands: pipes, substitutions, globs,
    /// and expansions go to review whatever this table says.
    // Each value is a [`CommandStance`] or the `bool` sugar ([`RawCommandValue`]).
    #[serde(default)]
    commands: BTreeMap<String, RawCommandValue>,
    /// Structured tools a call may use without review. Names match exactly, except that a
    /// trailing `.*` grants a namespace: `ferritin.*` for one server's tools, `efference.*`
    /// for efference's own. A server's tool may be written `ferritin__get` or
    /// `ferritin.get`. A tool that takes a path also needs that path granted in `[paths]`.
    #[serde(default)]
    tools: Vec<String>,
    /// Filesystem subtrees the structured tools may read or write. The longest matching
    /// prefix decides, so a deeper key carves an exception out of a broader one. Matching
    /// is lexical, without resolving symlinks: a guard against mistakes, not access
    /// control. Bash commands answer to `[commands]` instead.
    #[serde(default)]
    paths: BTreeMap<String, RawPathValue>,
    /// Reserved: parsed as configs to inherit from, with no effect.
    #[serde(default)]
    extends: Vec<String>,
    /// What a session starts out knowing, rendered into its preamble in the order written.
    /// Nothing is discovered automatically. A key is either a file path (`"CLAUDE.md" =
    /// true`) or a section label whose inline table names the source. Read once at boot;
    /// a listed file that cannot be read fails startup. A file with a frontmatter
    /// `description` rides as a one-line stub the session can read in full; one without
    /// rides whole.
    // An [`IndexMap`] (not `BTreeMap`) so document order is preserved: the entries render as
    // preamble sections in the order the operator wrote them. [`validate`] interprets each
    // value into a [`ContextEntry`].
    #[serde(default)]
    context: IndexMap<String, RawContextValue>,
    // `[mcp]`, `[bash]`, and `[transport]` carry no `///`: their hover text is their type's.
    // Taplo reads a `$ref`'s target and ignores a description beside it, so a field doc
    // here would show in Tombi only.
    #[serde(default)]
    mcp: RawMcp,
    #[serde(default)]
    bash: RawBash,
    #[serde(default)]
    transport: RawTransport,
    /// Per-variable environment policy: what reaches the session's subprocesses, what
    /// the log may store, and values to set. Keys are exact names or `*` globs, and an
    /// exact name beats a glob. `*_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`, and
    /// `*_CREDENTIALS` are masked by default.
    // docs/env.md. A bare `"*_KEY" = false` is a parse error: it cannot say which of the two
    // questions it answers.
    #[serde(default)]
    env: BTreeMap<String, crate::env_table::EnvEntry>,
}

/// `[bash]`: how long a foreground bash command may hold the turn before it moves to
/// the background, where it keeps running.
// docs/process-monitor.md. Two numbers, because one value cannot be both the default and the
// clamp. The ceiling is the loop-safety property, which is why it is operator-set and not
// call-set.
#[derive(Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Bash")]
pub(super) struct RawBash {
    /// Seconds a foreground command runs before it moves to the background, for a call
    /// that names no limit of its own. Default `300`.
    #[serde(rename = "background-after", default)]
    background_after: Option<u64>,
    /// The most seconds any one call may ask to stay in the foreground; a call's own
    /// limit is clamped to this. Default `1800`.
    #[serde(rename = "background-after-max", default)]
    background_after_max: Option<u64>,
}

/// `[transport]`: the connection to the model.
// docs/transport.md. Absent durations fall back to [`crate::api::DEFAULT_STREAM_CEILING`] and
// the provider's derived keepalive interval ([`crate::daemon::keepalive_interval`]).
#[derive(Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Transport")]
pub(super) struct RawTransport {
    /// Seconds one response's stream may stay open. Default `3600`.
    #[serde(rename = "stream-ceiling", default)]
    stream_ceiling: Option<u64>,
    /// Seconds the prompt cache may go untouched while the session is idle before a
    /// keepalive request touches it; `0` turns keepalives off. Runs only for a provider
    /// that reports a prompt cache. Default: chosen per provider.
    #[serde(rename = "keepalive-interval", default)]
    keepalive_interval: Option<u64>,
    /// How much effort the model spends per turn, such as `"low"`, `"medium"`, `"high"`,
    /// `"xhigh"`, or `"max"`. Checked at session start against the levels the model
    /// declares; omit it for the provider's default. `--effort` at session creation
    /// overrides it, and it cannot change mid-session.
    // Held as a string and passed through unchanged: the vocabulary is the model's metadata,
    // not a local enum, so any spelling parses and the boot check
    // (`ModelCapabilities::check_effort`) rejects one the model doesn't declare.
    #[serde(default)]
    effort: Option<String>,
}

/// `[mcp]`: MCP servers whose tools sessions here can call.
// A table (rather than hoisting `servers` to the top level) so mcp-wide knobs have a home.
#[derive(Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Mcp")]
pub(super) struct RawMcp {
    /// One `[mcp.servers.<name>]` table per server. A name uses `a-z`, `A-Z`, `0-9`, `_`,
    /// and `-`, is at most 32 characters, and contains no `__`: a server's tools are
    /// named `<server>__<tool>`.
    #[serde(default)]
    servers: BTreeMap<String, RawMcpServer>,
}

/// An MCP server: exactly one of `command` (a stdio subprocess) or `url` (a
/// streamable-HTTP endpoint). A server that fails to mount stops the session from
/// starting, unless it is `optional`.
// Which keys are present picks the transport — see [`McpTransport`]; [`validate_mcp_server`]
// rejects ambiguous or empty shapes.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "McpServer")]
pub(super) struct RawMcpServer {
    /// The program to run as a stdio server.
    #[serde(default)]
    command: Option<String>,
    /// Arguments to `command`.
    #[serde(default)]
    args: Vec<String>,
    /// Environment variables set for `command`.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// A streamable-HTTP endpoint. Takes no `command`, `args`, or `env`.
    #[serde(default)]
    url: Option<String>,
    /// Start the session even when this server fails to mount. The failure is reported to
    /// the operator and to the session. Default `false`.
    // See [`McpServerEntry::optional`].
    #[serde(default)]
    optional: bool,
}

/// A `[context]` entry. `true` lists the key as a read-only file. An inline table's keys
/// pick its kind: `path` or `paths` make a group headed by the entry's key, `exec` a
/// command's output, `builtin` documents compiled into efference. With none of those the
/// key is itself a file path, and only `update` may be set. `false` is refused.
// Interpreted by [`validate_context_entry`]. The union carries the hover text and its table arm
// none: Taplo prints every arm's description on hover, one after another. Deserialized through [`BoolOr`] so a misspelled
// key in the table is named; `#[schemars(untagged)]` gives the schema the same two spellings.
#[derive(JsonSchema)]
#[schemars(untagged, rename = "ContextValue")]
pub(super) enum RawContextValue {
    // `"./CLAUDE.md" = true` → a bare read-only path (the key is the path).
    Sugar(bool),
    // A labeled group (`{ path|paths = … }`), a dynamic section (`{ exec = … }`), a builtin, or
    // a bare path carrying `update` (the key is the path, no `path`/`paths`).
    Table(RawContextEntry),
}

impl<'de> Deserialize<'de> for RawContextValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match BoolOr::deserialize(deserializer)? {
            BoolOr::Bool(sugar) => RawContextValue::Sugar(sugar),
            BoolOr::Table(table) => RawContextValue::Table(table),
        })
    }
}

// The inline-table form of a `[context]` value; which kind it is is disambiguated by which keys
// are present — see [`validate_context_entry`]. Its hover text is `RawContextValue`'s.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "ContextEntry")]
pub(super) struct RawContextEntry {
    /// A file or directory for the group. A directory expands to every `.md` file beneath
    /// it, recursively.
    #[serde(default)]
    path: Option<String>,
    /// Files or directories for the group, as with `path`.
    #[serde(default)]
    paths: Vec<String>,
    /// A command run once at boot under `bash -c` in the session's directory; its stdout
    /// becomes the section, frozen for the session. It runs without review. Takes no
    /// other key.
    #[serde(default)]
    exec: Option<String>,
    /// Documents compiled into efference: `"guide"` lists them all as stubs, and
    /// `"guide/<name>"` puts one document's whole text in the preamble. Takes no other key.
    // Like `exec`, it names a source rather than files, so it mixes with nothing else
    // (docs/context.md, [`crate::guide`]).
    #[serde(default)]
    #[schemars(schema_with = "crate::guide::builtin_schema")]
    builtin: Option<String>,
    /// Let the session edit the listed files that exist. Default `false`.
    #[serde(default)]
    update: bool,
    /// Let the session add files under the group's directories; without `update`, the
    /// group is append-only. Group only. Default `false`.
    #[serde(default)]
    create: bool,
    /// The order of the group's files: comma-separated `<field> [asc|desc]` clauses over
    /// their frontmatter keys and the built-in `name` and `modified`, as in
    /// `"priority desc, name asc"`. Default `"name asc"`. Group only.
    #[serde(default)]
    sort: Option<String>,
    /// Paths left out of a directory's expansion, relative to each of the group's
    /// directories, as in `["MEMORY.md", "archive"]`. Group only.
    // A bare path carrying `exclude` is rejected in [`validate_context_entry`].
    #[serde(default)]
    exclude: Vec<String>,
    /// A note shown with the group telling the session how to use its files, such as how
    /// to read or write a memory. Group only.
    #[serde(default)]
    description: Option<String>,
}

/// The stance a command prefix gets. `"clear"` runs it without review. `"watch"` and
/// `"gate"` send it to review: the spotter, then the operator. `"deny"` skips the
/// spotter and goes to the operator, who can still allow it; an autonomous session
/// declines it. `true` is `"clear"`, and `false` is `"deny"`.
// Booleans parse because every config written before the stance vocabulary spells its grants
// that way; `false` is the carve-out spelling (`git = true` + `"git push" = false`) the
// longest-prefix resolution reads (docs/permissions.md).
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "CommandValue")]
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

/// Access to a subtree: `true` for read and write, `false` for none (a denial where it
/// is the longest matching prefix), or `{ read = …, write = … }`, where an omitted
/// permission is `false`.
// Deserialized through [`BoolOr`], like [`RawContextValue`].
#[derive(JsonSchema)]
#[schemars(untagged, rename = "PathValue")]
pub(super) enum RawPathValue {
    Sugar(bool),
    Perms(RawPerms),
}

impl<'de> Deserialize<'de> for RawPathValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match BoolOr::deserialize(deserializer)? {
            BoolOr::Bool(sugar) => RawPathValue::Sugar(sugar),
            BoolOr::Table(perms) => RawPathValue::Perms(perms),
        })
    }
}

// The inline-table form of a `[paths]` value; its hover text is `RawPathValue`'s.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "PathAccess")]
pub(super) struct RawPerms {
    /// Allow reading.
    #[serde(default)]
    read: bool,
    /// Allow writing, which implies reading.
    #[serde(default)]
    write: bool,
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
            RawPathValue::Perms(RawPerms { read, write }) => Perms { read, write },
        }
    }
}

/// A value spelled as the `bool` sugar or an inline table. Hand-written rather than
/// `#[serde(untagged)]` so a misspelled key inside the table is reported by name, through the
/// table's own `deny_unknown_fields`: an untagged enum tries each arm and reports only that
/// none matched.
enum BoolOr<T> {
    Bool(bool),
    Table(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for BoolOr<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BoolOrVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for BoolOrVisitor<T> {
            type Value = BoolOr<T>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("`true`, `false`, or an inline table")
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<BoolOr<T>, E> {
                Ok(BoolOr::Bool(v))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<BoolOr<T>, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(BoolOr::Table)
            }
        }
        deserializer.deserialize_any(BoolOrVisitor(std::marker::PhantomData))
    }
}

/// A failed validation of an otherwise-parseable TOML file — semantically wrong, not
/// syntactically. Carried by [`LoadError::Validate`].
#[derive(Debug)]
pub enum ValidationError {
    /// `first-turn = ""`: a kickoff with nothing in it. Omitting the key is how a
    /// session starts on the operator's turn instead.
    FirstTurnEmpty,
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
    /// A `[context]` `builtin` entry that also carries files, an `exec`, grants, or a sort —
    /// the source is ambiguous, and a compiled-in document set has no files to grant or sort.
    ContextBuiltinConflict(String),
    /// A `[context]` `builtin` naming a mount the binary does not ship. Rejected rather than
    /// rendering an empty section, which would read as "the harness has no documentation".
    ContextBuiltinUnknown(String, String),
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
            ValidationError::FirstTurnEmpty => write!(
                f,
                "`first-turn` is empty — remove the key for a session that waits for the \
                 operator's first message"
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
            ValidationError::ContextBuiltinConflict(key) => write!(
                f,
                "context entry `{key}` mixes `builtin` with `path`/`paths`/`exec`/grants/`sort` \
                 — a builtin entry lists documents compiled into the binary and has no files \
                 to grant or sort"
            ),
            ValidationError::ContextBuiltinUnknown(key, name) => write!(
                f,
                "context entry `{key}` names builtin `{name}`, which this binary does not ship \
                 — the builtins are `{}` (every document, listed) and `{}/<slug>` (one \
                 document, in full)",
                crate::guide::BUILTIN,
                crate::guide::BUILTIN
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
    if raw
        .first_turn
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        return Err(ValidationError::FirstTurnEmpty);
    }
    Ok(Config {
        scope: raw.scope,
        watch_for: raw.watch_for,
        trust_threshold: raw.trust_threshold,
        first_turn: raw.first_turn,
        consolidation_bytes: raw.consolidation_bytes,
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
            effort: raw.transport.effort,
            stream_ceiling: raw
                .transport
                .stream_ceiling
                .map_or(crate::api::DEFAULT_STREAM_CEILING, Duration::from_secs),
            keepalive_interval: raw.transport.keepalive_interval.map(Duration::from_secs),
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
            if let Some(builtin) = t.builtin {
                // Same rule as `exec`: the entry names a source that is not files, so
                // file grants, a sort, an exclude, and a second source are all
                // meaningless alongside it.
                if has_files
                    || t.exec.is_some()
                    || t.update
                    || t.create
                    || t.sort.is_some()
                    || !t.exclude.is_empty()
                {
                    return Err(ValidationError::ContextBuiltinConflict(key));
                }
                if !crate::guide::is_builtin(&builtin) {
                    return Err(ValidationError::ContextBuiltinUnknown(key, builtin));
                }
                return Ok(ContextEntry {
                    heading: Some(key),
                    source: ContextSource::Builtin(builtin),
                    grants: ContextGrants::default(),
                    sort: None,
                    exclude: Vec::new(),
                    description: t.description,
                });
            }
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
