//! Config — the cascade's foundational unit (backlog/permissions/README.md).
//!
//! A session is parameterized by `(config, cwd, orchestrator)`. The **config** is the
//! portable-policy half: what a session is *for* and what it may do, reusable across
//! projects and sessions. It is deliberately kept separate from `cwd` (the per-invocation
//! spatial scope, see [`crate::env`]) so the config stays portable — tweak in place or
//! fork by copy.
//!
//! ## The file is TOML, and it is a materialized table
//!
//! The file is TOML, parsed by [`toml_edit`]. We decode through a two-layer path: serde
//! deserializes the file into a [`RawConfig`] (the literal TOML shape), then [`validate`]
//! folds it into the [`Config`] the cascade actually consults — lexing every command key,
//! expanding the path sugar, and failing the whole file on any unsound entry.
//!
//! The schema is **string-keyed maps**, not lists, because the policy is a *table* the
//! config-edit loop mutates by key (backlog/permissions/config-edit.md): a later edit
//! **replaces at a key** (last-write-wins) rather than appending. That is why the persist
//! path can use [`toml_edit`]'s format-preserving writes — the harness edits the file *for*
//! the user at a gate, narrowing a single key while leaving every comment and unrelated
//! line untouched (the "yes, always" auto-persist append-only KDL couldn't express).
//!
//! ```toml
//! scope = "Work only inside this repo."
//! watch-for = ["network egress to non-registry hosts"]
//!
//! [commands]
//! cargo = true          # blanket — key lexes to ["cargo"]
//! "git diff" = true     # sub-verb — key lexes to ["git", "diff"]
//!
//! [paths]
//! "." = true            # sugar for { read = true, write = true }
//! "/refs" = { read = true }
//! ```
//!
//! ## What the config holds
//!
//! One artifact configures every cascade rung (backlog/permissions/README.md):
//!
//! - [`Config::scope`] — the positive scope, **shared with the acting session** for
//!   self-alignment. It projects into the cached preamble ([`Config::preamble_section`]).
//! - [`Config::watch_for`] — the spotter's "things to watch for", **spotter-only**.
//!   Kept off the session's preamble for attention-division (the focused session
//!   doesn't carry peripheral hazards; the spotter does). Consumed by the spotter
//!   rung ([`crate::spotter`]).
//! - [`Config::commands`] — the bash prefix-allowlist the deterministic fast-path matches
//!   against (backlog/permissions/prefix-fastpath.md). A **set** of command-prefix keys;
//!   membership *is* "clear" (the value will grow into a graded stance later — see
//!   backlog/permissions/config-edit.md). Consumed by that rung.
//! - [`Config::tools`] — structured tools the fast-path clears by name (also a set).
//! - [`Config::paths`] — filesystem subtree grants, the one genuine value-bearing map.
//! - [`Config::trust_threshold`] — where the watch-window/gate cutoffs sit. Consumed by
//!   the watch-window rung (not built yet).
//!
//! Per the convention in CLAUDE.md, the schema fields land *before* the rungs that
//! read them — "wire the seam ahead of the feature".
//!
//! ## Caching discipline (load-bearing)
//!
//! The scope projects into the system preamble, which sits in the cached prefix
//! (`tools → system → messages`). [`Config::preamble_section`] must therefore be
//! **byte-stable for the whole session** (see the invariant in
//! backlog/permissions/README.md, kin to `project(log)` in docs/substrate.md). It is —
//! the config is loaded once at startup and frozen, and the rendering is a pure function
//! of the scope text. Changing configs is a cold cache write, which is fine: configs do
//! not change mid-session.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Deserialize;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value, value};

use crate::log::ConfigChange;

/// The materialized policy table (backlog/permissions/config-edit.md). Built from the TOML
/// file by [`validate`]-ing a [`RawConfig`]; every field is optional/empty so a minimal — or
/// absent — file yields [`Config::default`]. This is what the cascade consults; the config-edit
/// fold mutates it by key ([`crate::policy::apply_config_change`]).
#[derive(Debug, Default, Clone)]
pub struct Config {
    /// Positive scope, shared with the acting session (self-alignment). Projects into the
    /// cached preamble via [`Config::preamble_section`]. TOML: `scope = "…"`.
    pub scope: Option<String>,

    /// The spotter's "things to watch for" — gotchas specific to this mission,
    /// **spotter-only**, never shown to the acting session (attention-division, not
    /// teach-to-the-test; the threat model is error, not deception). TOML:
    /// `watch-for = ["…", "…"]`. Consumed by the spotter rung.
    pub watch_for: Vec<String>,

    /// Trust posture → where the watch-window/gate cutoffs sit. `None` = unset (a real `0`
    /// would mean something specific, so absence is kept distinct). TOML:
    /// `trust-threshold = 0.5`. Consumed by the watch-window rung.
    pub trust_threshold: Option<f64>,

    /// The bash prefix-allowlist as a **set** of command-prefix keys
    /// (backlog/permissions/prefix-fastpath.md). Membership is the grant — `cargo` blanket-
    /// trusts the program, `git diff` trusts only that sub-verb. Each key is guaranteed (at
    /// [`validate`] time) to lex to a single simple-command token vector; [`crate::policy`]
    /// re-lexes at match time to compare token-prefixes. TOML `[commands]` table, all `true`.
    pub commands: BTreeSet<String>,

    /// Structured tools the fast-path clears by name, as a **set**. The path-bearing ones
    /// (`read`/`write`/`search`/edit) additionally require their target under a
    /// [`Config::paths`] grant; the daemon-internal ones clear on the name alone. Nothing is
    /// sanctioned in code — even `kill_process` is just an entry here. TOML: `tools = ["read"]`.
    pub tools: BTreeSet<String>,

    /// Filesystem permission grants — the one value-bearing map (backlog/permissions/README.md).
    /// Each key is a subtree, its [`Perms`] the read/write it grants; the fast-path clears a
    /// path-bearing tool only when its target resolves under a grant carrying the needed
    /// permission. cwd is not special in code — it is special only because the config says
    /// `"." = true`. TOML `[paths]` table; `"." = true` is sugar for `{ read = true, write = true }`.
    pub paths: BTreeMap<String, Perms>,

    /// Reserved ancestor pointers (tsconfig/eslint-style explicit inheritance). Parsed so a
    /// file can declare them, but the merge engine is **not built**. TOML: `extends = ["base.toml"]`.
    pub extends: Vec<String>,

    /// The `[context]` preamble builder (backlog/preamble-context.md): the markdown files (and, later,
    /// `exec` sections) the operator makes available to the session, in **document order** — a `Vec`,
    /// not a sorted map, because the entries render as preamble sections in the order they're written
    /// (a dynamic `exec` section can sit between static files). The harness bakes in nothing about
    /// what any entry "is" — see [`ContextEntry`].
    pub context: Vec<ContextEntry>,
}

/// One `[context]` entry — a source of preamble bytes with an optional heading, write grants, and a
/// sort override (backlog/preamble-context.md). Two axes land on two authorities: **mode**
/// (inline-vs-listed) is the file's call via its frontmatter, decided at render time; **grant**,
/// **heading**, and **sort** are the operator's call, materialized here from the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntry {
    /// The operator's section heading (`Memories`, `Skills`, `Runbooks`, …), or `None` for a bare
    /// path rendered with no heading. **Pure presentation** — the harness attaches no meaning to it.
    pub heading: Option<String>,
    /// What produces this section's bytes.
    pub source: ContextSource,
    /// Write grants beyond the always-on read: `update` (modify existing files), `create` (add new
    /// files to a location). `create` is group-only; a bare path can carry only `update`.
    pub grants: ContextGrants,
    /// Raw SQL-ish sort override for a multi-file source (`"modified desc"`); `None` = default
    /// `name asc`. Stored verbatim — parsed at render time (a later increment). Group-only.
    pub sort: Option<String>,
}

/// Where a [`ContextEntry`]'s bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSource {
    /// One or more file / dir / glob paths — a bare path (the table key) or a group's `path`/`paths`.
    Files(Vec<String>),
    /// A command whose stdout is the section body, run once at boot (backlog/dynamic-context-exec.md).
    Exec(String),
}

/// Write grants a [`ContextEntry`] carries (read is always implied for listed files).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextGrants {
    /// The session may modify existing file(s) at this entry. Legal on a bare path or a group.
    pub update: bool,
    /// The session may add new files to this location. Group-only (a single file has nowhere to
    /// create into).
    pub create: bool,
}

/// Read/write permission a [`Config::paths`] grant carries. `write` implies `read` at check
/// time (see [`crate::policy`]); both default `false`, so a bare `{ }` / `false` grants nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Perms {
    pub read: bool,
    pub write: bool,
}

/// The literal TOML shape, before validation. `deny_unknown_fields` makes a typo'd key fail
/// loudly — the schema is the contract. [`validate`] folds this into a [`Config`].
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    scope: Option<String>,
    #[serde(rename = "watch-for", default)]
    watch_for: Vec<String>,
    #[serde(rename = "trust-threshold", default)]
    trust_threshold: Option<f64>,
    /// `[commands]` table: each value is `true` (clear). `false` is rejected in [`validate`]
    /// (deny is unimplemented and the fold is additive-only — it can't subtract).
    #[serde(default)]
    commands: BTreeMap<String, bool>,
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
}

/// A `[context]` value: the `true`/`false` sugar (a bare read-only path) or an inline table
/// ([`RawContextEntry`]). Interpreted by [`validate_context_entry`].
#[derive(Deserialize)]
#[serde(untagged)]
enum RawContextValue {
    /// `"./CLAUDE.md" = true` → a bare read-only path (the key is the path).
    Sugar(bool),
    /// A table: a labeled group (`{ path|paths = … }`), a dynamic section (`{ exec = … }`), or a
    /// bare path carrying `update` (the key is the path, no `path`/`paths`).
    Table(RawContextEntry),
}

/// The inline-table form of a `[context]` value. Which shape it is (labeled group / exec / bare
/// updatable path) is disambiguated by which keys are present — see [`validate_context_entry`].
#[derive(Deserialize)]
struct RawContextEntry {
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
}

/// A `[paths]` value: either the `true`/`false` sugar (`true` = read+write) or an explicit
/// `{ read = …, write = … }` inline table.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawPathValue {
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
    /// A `[commands]` key set to `false`. Additive-only policy can't subtract, so a deny would
    /// silently do nothing — we reject it instead of surprising the author.
    CommandDeny(String),
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
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::CommandDeny(key) => write!(
                f,
                "command `{key}` is set to false, but command-deny is not implemented \
                 (config is additive-only); remove the entry instead"
            ),
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
        }
    }
}

/// Where a [`Config`] was resolved from, reported on `GET /state` so "which config am I
/// running?" is never ambiguous (backlog/permissions/README.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConfigSource {
    /// No config file found — the built-in [`Config::default`] is in effect.
    #[default]
    Default,
    /// Resolved from this path (an explicit `--config`, or the default path).
    Path(PathBuf),
}

/// A resolved config plus where it came from. Stored on the daemon's shared state and
/// reported by `/state`.
#[derive(Debug, Clone, Default)]
pub struct LoadedConfig {
    pub role: Config,
    pub source: ConfigSource,
}

/// The default config path, relative to cwd. Visible and committable (not under the
/// gitignored session dir) — a config is portable, shareable, not session state. The config
/// is *pointed-at*, not a magic scanned tree: only this one path is probed when `--config`
/// is absent.
pub const DEFAULT_PATH: &str = "efference.toml";

/// Failure to load a config file.
#[derive(Debug)]
pub enum LoadError {
    /// Reading the file failed (an explicit `--config` that doesn't exist, a permission
    /// error, …). A *missing default* path is not an error — it yields [`ConfigSource::Default`].
    Io(PathBuf, std::io::Error),
    /// The file is not valid TOML / does not match the schema. The [`toml_edit`] error carries
    /// the source span; its `Display` renders the offending line and a pointer.
    Parse(PathBuf, toml_edit::de::Error),
    /// The file parsed but is semantically unsound (see [`ValidationError`]).
    Validate(PathBuf, ValidationError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(path, e) => write!(f, "reading config {}: {e}", path.display()),
            LoadError::Parse(path, e) => write!(f, "invalid config {}: {e}", path.display()),
            LoadError::Validate(path, e) => write!(f, "invalid config {}: {e}", path.display()),
        }
    }
}

impl std::error::Error for LoadError {}

impl LoadError {
    /// Print this error to stderr. TOML parse errors render their own line/column context
    /// (the source-span pointer that stands in for KDL's old miette report); the others print
    /// their message. The leading `error:` keeps the shape the binary used before.
    pub fn eprint(&self) {
        eprintln!("error: {self}");
    }
}

/// Resolve and load the config. With `explicit` (a `--config` path) the file *must* exist —
/// a missing one is an error. Without it, the [`DEFAULT_PATH`] is probed: if present it loads,
/// if absent the built-in [`Config::default`] is used ([`ConfigSource::Default`]).
pub fn load(explicit: Option<&Path>) -> Result<LoadedConfig, LoadError> {
    match explicit {
        Some(path) => {
            let text =
                std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.to_path_buf(), e))?;
            Ok(LoadedConfig {
                role: parse(path, &text)?,
                source: ConfigSource::Path(path.to_path_buf()),
            })
        }
        None => {
            let path = Path::new(DEFAULT_PATH);
            match std::fs::read_to_string(path) {
                Ok(text) => Ok(LoadedConfig {
                    role: parse(path, &text)?,
                    source: ConfigSource::Path(path.to_path_buf()),
                }),
                // The default path is optional: absent → the built-in default config.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LoadedConfig::default()),
                Err(e) => Err(LoadError::Io(path.to_path_buf(), e)),
            }
        }
    }
}

/// Parse TOML text into a [`Config`] (decode → validate). The `path` labels diagnostics only.
fn parse(path: &Path, text: &str) -> Result<Config, LoadError> {
    let raw: RawConfig =
        toml_edit::de::from_str(text).map_err(|e| LoadError::Parse(path.to_path_buf(), e))?;
    validate(raw).map_err(|e| LoadError::Validate(path.to_path_buf(), e))
}

/// Fold a [`RawConfig`] into the materialized [`Config`], failing closed on any unsound entry.
/// The two soundness gates live here: every `[commands]` key must lex to a simple command
/// (reusing the fast-path's own lexer so the key and the checked command agree token-for-token),
/// and a `false` command value is rejected (additive-only can't deny).
fn validate(raw: RawConfig) -> Result<Config, ValidationError> {
    let mut commands = BTreeSet::new();
    for (key, granted) in raw.commands {
        if !granted {
            return Err(ValidationError::CommandDeny(key));
        }
        if crate::policy::simple_command_tokens(&key).is_none() {
            return Err(ValidationError::CommandNotSimple(key));
        }
        commands.insert(key);
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
    Ok(Config {
        scope: raw.scope,
        watch_for: raw.watch_for,
        trust_threshold: raw.trust_threshold,
        commands,
        tools: raw.tools.into_iter().collect(),
        paths,
        extends: raw.extends,
        context,
    })
}

/// Interpret one `[context]` `(key, value)` into a [`ContextEntry`], failing closed on an illegal
/// shape (backlog/preamble-context.md). The value's shape decides the kind:
///
/// - `true` → a bare read-only path (the key is the path, no heading).
/// - a table with `exec` → a labeled dynamic section (no files/grants/sort allowed alongside).
/// - a table with `path`/`paths` → a labeled group (may carry `update`/`create`/`sort`).
/// - a table with neither → a bare path (the key is the path) that may carry only `update`;
///   `create`/`sort` are group-only and rejected here.
fn validate_context_entry(
    key: String,
    value: RawContextValue,
) -> Result<ContextEntry, ValidationError> {
    let entry = match value {
        RawContextValue::Sugar(true) => ContextEntry {
            heading: None,
            source: ContextSource::Files(vec![key]),
            grants: ContextGrants::default(),
            sort: None,
        },
        RawContextValue::Sugar(false) => return Err(ValidationError::ContextDisabled(key)),
        RawContextValue::Table(t) => {
            let has_files = t.path.is_some() || !t.paths.is_empty();
            if let Some(exec) = t.exec {
                // A dynamic section is a labeled group whose body is command stdout: it has no
                // files, so file grants and a sort would be meaningless — reject the mix.
                if has_files || t.update || t.create || t.sort.is_some() {
                    return Err(ValidationError::ContextExecConflict(key));
                }
                ContextEntry {
                    heading: Some(key),
                    source: ContextSource::Exec(exec),
                    grants: ContextGrants::default(),
                    sort: None,
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
                ContextEntry {
                    heading: None,
                    source: ContextSource::Files(vec![key]),
                    grants: ContextGrants {
                        update: t.update,
                        create: false,
                    },
                    sort: None,
                }
            }
        }
    };
    Ok(entry)
}

/// The additive/replace deltas that carry `from` to `to`, as [`ConfigChange`]s — the diff the
/// fs-watch initiator emits when the config file changes (backlog/permissions/proposal-lifecycle.md).
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

/// Persist a committed [`ConfigChange`] to the config file by editing it in place via
/// [`toml_edit`] (backlog/permissions/config-edit.md). Unlike the old append-only KDL render,
/// this **replaces at a key** while preserving every comment and unrelated line — the property
/// that makes "yes, always" auto-persist sound for *every* slot, including a single `scope`
/// narrowing. Idempotent by construction: setting a key/array-member that already holds the
/// value is a no-op on the wire.
pub fn persist_config_change(path: &Path, change: &ConfigChange) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
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
            if let Some(arr) = entry.as_array_mut() {
                if !arr.iter().any(|v| v.as_str() == Some(name.as_str())) {
                    arr.push(name.as_str());
                }
            }
        }
        // A scope replace: set the `scope` scalar in place. When it already exists, carry its
        // decor over so the author's inline comment survives the narrowing (the auto-persist
        // guarantee; cf. backlog/permissions/config-edit.md).
        ConfigChange::Scope { text } => match doc.get_mut("scope").and_then(Item::as_value_mut) {
            Some(existing) => {
                let decor = existing.decor().clone();
                *existing = Value::from(text.as_str());
                *existing.decor_mut() = decor;
            }
            None => doc["scope"] = value(text.as_str()),
        },
    }
    std::fs::write(path, doc.to_string())
}

/// Render a `[paths]` value: the `true` sugar when read+write, otherwise an inline table
/// carrying exactly the granted perms (`{ read = true }`), or `false` when nothing is granted.
fn path_value(read: bool, write: bool) -> Item {
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

impl Config {
    /// Render the scope into the byte-stable preamble section appended to the system prompt
    /// (backlog/permissions/README.md), or `None` if there is no scope. The header is fixed
    /// and the body is the scope verbatim (trimmed), so the output is a pure, deterministic
    /// function of the scope — safe for the cached prefix. The watch-for list is deliberately
    /// *not* projected here: it is spotter-only.
    pub fn preamble_section(&self) -> Option<String> {
        let scope = self
            .scope
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())?;
        Some(format!("Your role for this session:\n\n{scope}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> Config {
        super::parse(Path::new("test.toml"), text).expect("valid config")
    }

    #[test]
    fn empty_document_is_a_valid_default_config() {
        let role = parse_str("");
        assert!(role.scope.is_none());
        assert!(role.watch_for.is_empty());
        assert!(role.trust_threshold.is_none());
        assert!(role.commands.is_empty());
        assert!(role.tools.is_empty());
        assert!(role.paths.is_empty());
        assert!(role.extends.is_empty());
        assert!(role.context.is_empty());
    }

    #[test]
    fn parses_scope_watch_for_and_trust_threshold() {
        let role = parse_str(
            r#"
            scope = "Work only inside this repo."
            watch-for = ["Flag network egress to non-registry hosts."]
            trust-threshold = 0.5
            "#,
        );
        assert_eq!(role.scope.as_deref(), Some("Work only inside this repo."));
        assert_eq!(
            role.watch_for,
            ["Flag network egress to non-registry hosts."]
        );
        assert_eq!(role.trust_threshold, Some(0.5));
    }

    #[test]
    fn parses_blanket_and_sub_verb_command_keys() {
        let role = parse_str(
            r#"
            [commands]
            cargo = true
            "git diff" = true
            "git status" = true
            "#,
        );
        assert!(role.commands.contains("cargo"));
        assert!(role.commands.contains("git diff"));
        assert!(role.commands.contains("git status"));
        assert_eq!(role.commands.len(), 3);
    }

    #[test]
    fn parses_tool_grants() {
        let role = parse_str(r#"tools = ["read", "write", "kill_process"]"#);
        assert!(role.tools.contains("read"));
        assert!(role.tools.contains("write"));
        assert!(role.tools.contains("kill_process"));
    }

    #[test]
    fn parses_path_grants_with_sugar_and_explicit_perms() {
        let role = parse_str(
            r#"
            [paths]
            "." = true
            "/tmp" = { read = true }
            "vendor" = false
            "#,
        );
        assert_eq!(role.paths.len(), 3);
        // cwd sugar → full access.
        assert_eq!(
            role.paths["."],
            Perms {
                read: true,
                write: true
            }
        );
        // /tmp: read-only (write defaults false).
        assert_eq!(
            role.paths["/tmp"],
            Perms {
                read: true,
                write: false
            }
        );
        // `false` grants nothing until you say what you're granting.
        assert_eq!(
            role.paths["vendor"],
            Perms {
                read: false,
                write: false
            }
        );
    }

    #[test]
    fn parses_reserved_extends_pointer() {
        let role = parse_str(r#"extends = ["base.toml"]"#);
        assert_eq!(role.extends, ["base.toml"]);
    }

    // --- [context] preamble builder (backlog/preamble-context.md) ---

    /// The three legal entry shapes decode to the right `ContextEntry`: a bare read-only path, a
    /// bare updatable path (key-is-path table), and a labeled group with `path` + write grants.
    #[test]
    fn parses_context_entry_shapes() {
        let role = parse_str(
            r#"
            [context]
            "./CLAUDE.md" = true
            "./ARCHITECTURE.md" = { update = true }
            Memories = { path = "./memories/", create = true, update = true }
            "#,
        );
        assert_eq!(
            role.context,
            vec![
                ContextEntry {
                    heading: None,
                    source: ContextSource::Files(vec!["./CLAUDE.md".into()]),
                    grants: ContextGrants::default(),
                    sort: None,
                },
                ContextEntry {
                    heading: None,
                    source: ContextSource::Files(vec!["./ARCHITECTURE.md".into()]),
                    grants: ContextGrants {
                        update: true,
                        create: false,
                    },
                    sort: None,
                },
                ContextEntry {
                    heading: Some("Memories".into()),
                    source: ContextSource::Files(vec!["./memories/".into()]),
                    grants: ContextGrants {
                        update: true,
                        create: true,
                    },
                    sort: None,
                },
            ],
        );
    }

    /// A labeled group's `paths` list and a `sort` override decode; `path` then `paths` join in order.
    #[test]
    fn parses_context_group_with_paths_and_sort() {
        let role = parse_str(
            r#"
            [context]
            Skills = { paths = ["a/*.md", "b/*.md"], sort = "modified desc" }
            "#,
        );
        assert_eq!(
            role.context,
            vec![ContextEntry {
                heading: Some("Skills".into()),
                source: ContextSource::Files(vec!["a/*.md".into(), "b/*.md".into()]),
                grants: ContextGrants::default(),
                sort: Some("modified desc".into()),
            }],
        );
    }

    /// An `exec` entry is a labeled dynamic section with no files, grants, or sort.
    #[test]
    fn parses_context_exec_entry() {
        let role = parse_str(
            r#"
            [context]
            Date = { exec = "date" }
            "#,
        );
        assert_eq!(
            role.context,
            vec![ContextEntry {
                heading: Some("Date".into()),
                source: ContextSource::Exec("date".into()),
                grants: ContextGrants::default(),
                sort: None,
            }],
        );
    }

    /// **Document order is preserved** — the `[context]` entries come out in file order, not sorted
    /// (they render as preamble sections positionally; an `exec` can sit between static files). The
    /// keys here are deliberately anti-alphabetical to catch a sorted-map regression.
    #[test]
    fn context_preserves_document_order() {
        let role = parse_str(
            r#"
            [context]
            "zzz.md" = true
            Middle = { exec = "date" }
            "aaa.md" = true
            "#,
        );
        let headings_and_sources: Vec<_> = role
            .context
            .iter()
            .map(|e| (e.heading.clone(), e.source.clone()))
            .collect();
        assert_eq!(
            headings_and_sources,
            vec![
                (None, ContextSource::Files(vec!["zzz.md".into()])),
                (Some("Middle".into()), ContextSource::Exec("date".into())),
                (None, ContextSource::Files(vec!["aaa.md".into()])),
            ],
        );
    }

    /// `create` on a bare path (key-is-path, no `path`/`paths`) fails — it's group-only.
    #[test]
    fn context_create_on_bare_path_fails() {
        let err = super::parse(
            Path::new("bad.toml"),
            "[context]\n\"./notes.md\" = { create = true }\n",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::ContextBarePathGrant(_, "create"))
        ));
    }

    /// `sort` on a bare path fails for the same reason — nothing to order.
    #[test]
    fn context_sort_on_bare_path_fails() {
        let err = super::parse(
            Path::new("bad.toml"),
            "[context]\n\"./notes.md\" = { sort = \"modified desc\" }\n",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::ContextBarePathGrant(_, "sort"))
        ));
    }

    /// `exec` mixed with a file source is ambiguous and fails.
    #[test]
    fn context_exec_with_files_fails() {
        let err = super::parse(
            Path::new("bad.toml"),
            "[context]\nBad = { exec = \"date\", path = \"./x\" }\n",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::ContextExecConflict(_))
        ));
    }

    /// A `false` context entry is rejected — nothing to disable.
    #[test]
    fn context_false_is_rejected() {
        let err =
            super::parse(Path::new("bad.toml"), "[context]\n\"./x.md\" = false\n").unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::ContextDisabled(_))
        ));
    }

    #[test]
    fn malformed_toml_is_a_parse_error_not_a_panic() {
        let err = super::parse(Path::new("bad.toml"), "scope = ").unwrap_err();
        assert!(matches!(err, LoadError::Parse(..)));
    }

    #[test]
    fn unknown_key_is_rejected() {
        // A typo'd key should fail loudly rather than be silently ignored — the typed schema
        // is the contract (deny_unknown_fields).
        let err = super::parse(Path::new("bad.toml"), r#"breif = "typo""#).unwrap_err();
        assert!(matches!(err, LoadError::Parse(..)));
    }

    /// The soundness gate: a `[commands]` key that isn't a single simple command fails the
    /// whole file — no silent skip, no partial recovery.
    #[test]
    fn non_simple_command_key_fails_the_file() {
        let err =
            super::parse(Path::new("bad.toml"), "[commands]\n\"git | rm\" = true\n").unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::CommandNotSimple(_))
        ));
    }

    /// A `false` command value is rejected (additive-only can't deny).
    #[test]
    fn command_deny_is_rejected() {
        let err = super::parse(Path::new("bad.toml"), "[commands]\ncargo = false\n").unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::CommandDeny(_))
        ));
    }

    #[test]
    fn preamble_section_renders_the_scope_byte_for_byte() {
        let role = parse_str(r#"scope = "Stay in the repo.""#);
        assert_eq!(
            role.preamble_section().as_deref(),
            Some("Your role for this session:\n\nStay in the repo.")
        );
    }

    #[test]
    fn preamble_section_is_none_without_a_scope() {
        assert!(Config::default().preamble_section().is_none());
        // A whitespace-only scope is treated as no scope (no empty section on the wire).
        let role = parse_str(r#"scope = "   ""#);
        assert!(role.preamble_section().is_none());
    }

    /// The watch-for list is spotter-only — it must never leak into the session's preamble.
    #[test]
    fn preamble_section_excludes_the_watch_for() {
        let role = parse_str(
            r#"
            scope = "Do the task."
            watch-for = ["SECRET RED FLAGS"]
            "#,
        );
        let section = role.preamble_section().unwrap();
        assert!(section.contains("Do the task."));
        assert!(!section.contains("SECRET RED FLAGS"));
    }

    #[test]
    fn explicit_missing_config_is_an_io_error() {
        let missing =
            std::env::temp_dir().join(format!("efference-no-such-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let err = load(Some(&missing)).unwrap_err();
        assert!(
            matches!(err, LoadError::Io(_, _)),
            "explicit path must exist"
        );
    }

    #[test]
    fn explicit_present_config_resolves_with_its_path() {
        let path =
            std::env::temp_dir().join(format!("efference-config-{}.toml", std::process::id()));
        std::fs::write(&path, r#"scope = "hi""#).unwrap();
        let loaded = load(Some(&path)).unwrap();
        assert_eq!(loaded.role.scope.as_deref(), Some("hi"));
        assert_eq!(loaded.source, ConfigSource::Path(path.clone()));
        std::fs::remove_file(&path).unwrap();
    }

    /// Persisting a command change sets `[commands][key] = true`, preserves unrelated comments
    /// and formatting, round-trips back through the loader, and is idempotent on re-persist.
    #[test]
    fn persist_command_sets_key_preserves_comments_and_round_trips() {
        let path =
            std::env::temp_dir().join(format!("efference-persist-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "# my config\nscope = \"do the thing\"  # the mission\n\n[commands]\ncargo = true\n",
        )
        .unwrap();

        let change = ConfigChange::Command {
            command: "git diff".into(),
        };
        persist_config_change(&path, &change).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"), "comment preserved: {text}");
        assert!(
            text.contains("# the mission"),
            "inline comment preserved: {text}"
        );
        assert!(text.contains(r#""git diff" = true"#), "key set: {text}");

        // Round-trips back through the loader into the command set.
        let role = parse(&path, &text).unwrap();
        assert!(role.commands.contains("git diff"));
        assert!(role.commands.contains("cargo"));

        // Re-persisting the same change is a no-op — no duplicate key.
        persist_config_change(&path, &change).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after.matches(r#""git diff" = true"#).count(),
            1,
            "idempotent: {after}"
        );

        std::fs::remove_file(&path).unwrap();
    }

    /// Persisting into an **empty** file vivifies the missing sections — the case the gating
    /// test (which edited an existing `[commands]`) didn't cover. Tool grants append to the
    /// `tools` array and dedup on membership; path grants render the sugar / inline perms.
    #[test]
    fn persist_into_empty_file_vivifies_sections() {
        let path = std::env::temp_dir().join(format!(
            "efference-persist-empty-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "").unwrap();

        persist_config_change(
            &path,
            &ConfigChange::Tool {
                name: "read".into(),
            },
        )
        .unwrap();
        persist_config_change(
            &path,
            &ConfigChange::Tool {
                name: "read".into(),
            },
        )
        .unwrap();
        persist_config_change(
            &path,
            &ConfigChange::Path {
                path: "/refs".into(),
                read: true,
                write: false,
            },
        )
        .unwrap();
        persist_config_change(
            &path,
            &ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true,
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let role = parse(&path, &text).unwrap();
        // The tool grant landed once (membership dedup, not last-write-wins).
        assert_eq!(
            role.tools.iter().filter(|t| *t == "read").count(),
            1,
            "{text}"
        );
        // Path sugar + inline perms both decode back.
        assert_eq!(
            role.paths["."],
            Perms {
                read: true,
                write: true
            },
            "{text}"
        );
        assert_eq!(
            role.paths["/refs"],
            Perms {
                read: true,
                write: false
            },
            "{text}"
        );

        std::fs::remove_file(&path).unwrap();
    }

    /// A `Scope` change replaces the scalar in place (preserving its inline comment) when present,
    /// and inserts it when absent — the narrowing edit append-only KDL couldn't express.
    #[test]
    fn persist_scope_replaces_in_place_then_inserts_when_absent() {
        let path = std::env::temp_dir().join(format!(
            "efference-persist-scope-{}.toml",
            std::process::id()
        ));

        // Replace in place: the inline comment on the scope line survives.
        std::fs::write(
            &path,
            "scope = \"old\"  # the perimeter\n[commands]\ncargo = true\n",
        )
        .unwrap();
        persist_config_change(
            &path,
            &ConfigChange::Scope {
                text: "narrowed".into(),
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(r#"scope = "narrowed""#),
            "scope replaced: {text}"
        );
        assert!(
            text.contains("# the perimeter"),
            "inline comment preserved: {text}"
        );
        assert!(
            text.contains("cargo = true"),
            "unrelated section intact: {text}"
        );
        assert_eq!(
            parse(&path, &text).unwrap().scope.as_deref(),
            Some("narrowed")
        );

        // Insert when absent: a file with no scope gains one.
        std::fs::write(&path, "[commands]\ncargo = true\n").unwrap();
        persist_config_change(
            &path,
            &ConfigChange::Scope {
                text: "fresh".into(),
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            parse(&path, &text).unwrap().scope.as_deref(),
            Some("fresh"),
            "{text}"
        );

        std::fs::remove_file(&path).unwrap();
    }

    // --- diff_changes: the fs-watch initiator's file-vs-effective delta ---

    /// New keys the file adds over the effective config surface as additive changes — one per slot,
    /// in deterministic (sorted) order; a scope change replaces the scalar.
    #[test]
    fn diff_emits_additive_changes_for_what_the_file_adds() {
        let from = parse_str("scope = \"old\"\n[commands]\ncargo = true\n");
        let to = parse_str(
            "scope = \"new\"\ntools = [\"read\"]\n\
             [commands]\ncargo = true\n\"git diff\" = true\n[paths]\n\".\" = true\n",
        );
        let changes = diff_changes(&from, &to);
        assert_eq!(
            changes,
            vec![
                ConfigChange::Scope { text: "new".into() },
                ConfigChange::Command {
                    command: "git diff".into()
                },
                ConfigChange::Tool {
                    name: "read".into()
                },
                ConfigChange::Path {
                    path: ".".into(),
                    read: true,
                    write: true
                },
            ],
        );
    }

    /// Diffing a config against itself is empty — the property that makes the harness's own
    /// auto-persist write-back a harmless no-op echo (the file already matches effective).
    #[test]
    fn diff_against_self_is_empty() {
        let cfg = parse_str("scope = \"s\"\ntools = [\"read\"]\n[commands]\ncargo = true\n");
        assert!(diff_changes(&cfg, &cfg).is_empty());
    }

    /// A *changed* path perm surfaces (write added); a removed key does **not** (the additive fold
    /// can't express removal — tightening waits for the next boot, which reloads the file as base).
    #[test]
    fn diff_emits_perm_changes_but_not_removals() {
        let from =
            parse_str("[commands]\ncargo = true\nnpm = true\n[paths]\n\".\" = { read = true }\n");
        let to = parse_str("[commands]\ncargo = true\n[paths]\n\".\" = true\n");
        let changes = diff_changes(&from, &to);
        // `.` gained write — emitted. `npm` was removed — not emitted (additive only).
        assert_eq!(
            changes,
            vec![ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true
            }],
        );
    }
}
