//! Config — the cascade's foundational unit (docs/permissions.md).
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
//! config-edit loop mutates by key (docs/permissions.md): a later edit
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
//! One artifact configures every cascade rung (docs/permissions.md):
//!
//! - [`Config::scope`] — the positive scope, **shared with the acting session** for
//!   self-alignment. It projects into the cached preamble ([`Config::preamble_section`]).
//! - [`Config::watch_for`] — the spotter's "things to watch for", **spotter-only**.
//!   Kept off the session's preamble for attention-division (the focused session
//!   doesn't carry peripheral hazards; the spotter does). Consumed by the spotter
//!   rung ([`crate::spotter`]).
//! - [`Config::commands`] — the bash prefix-allowlist the deterministic fast-path matches
//!   against (docs/permissions.md). A **set** of command-prefix keys;
//!   membership *is* "clear" (the value will grow into a graded stance later — see
//!   docs/permissions.md). Consumed by that rung.
//! - [`Config::tools`] — structured tools the fast-path clears by name (also a set).
//! - [`Config::paths`] — filesystem subtree grants, the one genuine value-bearing map.
//! - [`Config::trust_threshold`] — where the watch-window/gate cutoffs sit. Consumed by
//!   the watch-window rung (not built yet).
//!
//! Per the convention in CLAUDE.md, the schema fields land *before* the rungs that
//! read them — types land ahead of the feature that constructs them.
//!
//! ## Caching discipline
//!
//! The scope projects into the system preamble, which sits in the cached prefix
//! (`tools → system → messages`). [`Config::preamble_section`] must therefore be
//! **byte-stable for the whole session** (see the invariant in
//! docs/permissions.md, kin to `project(log)` in docs/log-and-projection.md). It is —
//! the config is loaded once at startup and frozen, and the rendering is a pure function
//! of the scope text. Changing configs is a cold cache write, which is fine: configs do
//! not change mid-session.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Deserialize;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value, value};

use crate::log::ConfigChange;

/// The materialized policy table (docs/permissions.md). Built from the TOML
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
    /// (docs/permissions.md). Membership is the grant — `cargo` blanket-
    /// trusts the program, `git diff` trusts only that sub-verb. Each key is guaranteed (at
    /// [`validate`] time) to lex to a single simple-command token vector; [`crate::policy`]
    /// re-lexes at match time to compare token-prefixes. TOML `[commands]` table, all `true`.
    pub commands: BTreeSet<String>,

    /// Structured tools the fast-path clears by name, as a **set**. The path-bearing ones
    /// (`read`/`write`/`search`/edit) additionally require their target under a
    /// [`Config::paths`] grant; the daemon-internal ones clear on the name alone. Nothing is
    /// sanctioned in code — even `kill_process` is just an entry here. TOML: `tools = ["read"]`.
    pub tools: BTreeSet<String>,

    /// Filesystem permission grants — the one value-bearing map (docs/permissions.md).
    /// Each key is a subtree, its [`Perms`] the read/write it grants; the fast-path clears a
    /// path-bearing tool only when its target resolves under a grant carrying the needed
    /// permission. cwd is not special in code — it is special only because the config says
    /// `"." = true`. TOML `[paths]` table; `"." = true` is sugar for `{ read = true, write = true }`.
    pub paths: BTreeMap<String, Perms>,

    /// Reserved ancestor pointers (tsconfig/eslint-style explicit inheritance). Parsed so a
    /// file can declare them, but the merge engine is **not built**. TOML: `extends = ["base.toml"]`.
    pub extends: Vec<String>,

    /// The `[context]` preamble builder (docs/context.md): the markdown files (and, later,
    /// `exec` sections) the operator makes available to the session, in **document order** — a `Vec`,
    /// not a sorted map, because the entries render as preamble sections in the order they're written
    /// (a dynamic `exec` section can sit between static files). The harness bakes in nothing about
    /// what any entry "is" — see [`ContextEntry`].
    pub context: Vec<ContextEntry>,
}

/// One `[context]` entry — a source of preamble bytes with an optional heading, write grants, and a
/// sort override (docs/context.md). Two axes land on two authorities: **mode**
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
    /// A command whose stdout is the section body, run once at boot (docs/context.md).
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
/// running?" is never ambiguous (docs/permissions.md).
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

/// The live config file: the format-preserving TOML document currently on disk, plus the
/// [`Config`] it validates to (docs/permissions.md).
///
/// ## The invariant
///
/// **The held document is the last content that parsed, and the disk always holds it.** The file
/// is the session's policy interface — the operator reads it to see the perimeter the session ran
/// under, and copies grants up to the origin template from it — so it must never sit in a state
/// that doesn't parse. A config edit therefore has exactly two outcomes, never a silent third
/// ("approved but didn't take"): **adopted**, or **rejected and reverted with the reason**.
///
/// Two directions maintain that invariant, and reverting is not a separate mechanism — it is the
/// same [`sync`] the harness's own edits use:
///
/// - [`apply`] + [`sync`] (memory → disk): mutate the document at a key, write it out.
/// - [`refresh`] (disk → memory): re-read. On a parse, **replace** the held document wholesale (a
///   merge would resurrect keys the author deleted). On a failure, [`sync`] the last-good document
///   back over the file.
///
/// Holding [`DocumentMut`] rather than the file's text is what makes [`apply`] format-preserving
/// without a re-read, and what makes an unmodified document round-trip to its original bytes —
/// every comment, key order, and blank line survives both an edit and a revert. That matters
/// concretely: the parse-error message tells the model to consult the schema header comment, so a
/// revert that dropped comments would erase its own advice.
///
/// [`apply`]: ConfigFile::apply
/// [`sync`]: ConfigFile::sync
/// [`refresh`]: ConfigFile::refresh
#[derive(Debug)]
pub struct ConfigFile {
    path: PathBuf,
    /// The last content that parsed, as a format-preserving AST.
    doc: DocumentMut,
    /// What [`Self::doc`] validates to. Kept alongside so a reader needn't re-parse.
    config: Config,
}

/// The outcome of a [`ConfigFile::refresh`] — which of the two permitted outcomes the edit got.
#[derive(Debug)]
pub enum Refreshed {
    /// The file parsed; the held document now matches disk. Carries the newly-effective config
    /// for the caller to diff against the policy in force.
    Adopted(Config),
    /// The file did not parse; the last-good document was written back over it, so disk is valid
    /// again and the effective policy is unchanged. `error` is the parse failure, verbatim, to be
    /// delivered to whoever authored the edit.
    Reverted { error: String },
    /// The file did not parse **and** the write-back failed — the one state where disk is left
    /// invalid. Reported honestly rather than swallowed: the caller cannot claim a revert.
    RevertFailed { error: String, io: std::io::Error },
}

impl ConfigFile {
    /// Read and parse `path` into a live document. Fails if it doesn't parse — a session never
    /// starts on an invalid config (boot already fails closed), which is what guarantees the held
    /// document exists for every later revert.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.to_path_buf(), e))?;
        let config = parse(path, &text)?;
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| LoadError::Parse(path.to_path_buf(), toml_edit::de::Error::from(e)))?;
        Ok(Self {
            path: path.to_path_buf(),
            doc,
            config,
        })
    }

    /// The config the held document validates to — the file's contribution to policy.
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the held document to disk. This is both the persist path and the revert path: since
    /// the document is by construction the last content that parsed, writing it out is how the
    /// file returns to validity, whatever put it in an invalid state.
    pub fn sync(&self) -> std::io::Result<()> {
        std::fs::write(&self.path, self.doc.to_string())
    }

    /// Apply a committed [`ConfigChange`] to the held document and write it out
    /// (docs/permissions.md). **Replaces at a key** while preserving every comment
    /// and unrelated line — the property that makes "yes, always" auto-persist sound for *every*
    /// slot, including a single `scope` narrowing. Idempotent by construction: setting a
    /// key/array-member that already holds the value is a no-op on disk.
    ///
    /// The mutated document is re-validated before it is written, so the invariant ("the held
    /// document parses") cannot be broken by the harness's own edit; a change that somehow
    /// wouldn't parse is rejected with the document untouched rather than written out.
    pub fn apply(&mut self, change: &ConfigChange) -> std::io::Result<()> {
        let mut doc = self.doc.clone();
        apply_to_document(&mut doc, change);
        let config = parse(&self.path, &doc.to_string())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        self.doc = doc;
        self.config = config;
        self.sync()
    }

    /// Re-read the file: **adopt** it if it parses, **revert** it if it doesn't
    /// (docs/permissions.md). The single entry point for every way the
    /// file can change — a model's gated `write`/`edit`, an operator's hand-edit, the harness's own
    /// write-back echo — because the invariant is about the file's state, not about who authored it.
    ///
    /// An unreadable file (deleted, renamed away mid-save) takes the same path as an unparseable
    /// one: the held document is written back, restoring it.
    pub fn refresh(&mut self) -> Refreshed {
        let reverted = |file: &Self, error: String| match file.sync() {
            Ok(()) => Refreshed::Reverted { error },
            Err(io) => Refreshed::RevertFailed { error, io },
        };
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) => {
                let error = LoadError::Io(self.path.clone(), e).to_string();
                return reverted(self, error);
            }
        };
        let config = match parse(&self.path, &text) {
            Ok(config) => config,
            Err(e) => return reverted(self, e.to_string()),
        };
        // Parsed as a `Config`, so it parses as a document; a failure here would mean the two
        // parsers disagree, which is a revert like any other invalid state.
        let doc = match text.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(e) => return reverted(self, e.to_string()),
        };
        self.doc = doc;
        self.config = config.clone();
        Refreshed::Adopted(config)
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
/// shape (docs/context.md). The value's shape decides the kind:
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
fn apply_to_document(doc: &mut DocumentMut, change: &ConfigChange) {
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
    /// (docs/permissions.md), or `None` if there is no scope. The header is fixed
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
mod test;
