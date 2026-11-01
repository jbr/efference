//! Config — the cascade's foundational unit (docs/permissions.md).
//!
//! A session is parameterized by `(config, cwd, orchestrator)`. The **config** is the
//! portable-policy half: what a session is *for* and what it may do, reusable across
//! projects and sessions. It is deliberately kept separate from `cwd` (the per-invocation
//! spatial scope, see [`crate::config::env`]) so the config stays portable — tweak in place or
//! fork by copy.
//!
//! ## The file is TOML, and it is a materialized table
//!
//! The file is TOML, parsed by [`toml_edit`]. We decode through a two-layer path: serde
//! deserializes the file into a [`RawConfig`] (the literal TOML shape), then
//! [`validate`](raw::validate) folds it into the [`Config`] the cascade actually
//! consults — lexing every command key, expanding the path sugar, and failing the whole
//! file on any unsound entry.
//!
//! The schema is **string-keyed maps**, not lists, because the policy is a *table* the
//! config-edit loop mutates by key (docs/permissions.md): a later edit
//! **replaces at a key** (last-write-wins) rather than appending. That is why the persist
//! path can use [`toml_edit`]'s format-preserving writes — the harness edits the file *for*
//! the user at a gate, narrowing a single key while leaving every comment and unrelated
//! line untouched.
//!
//! ```toml
//! scope = "Work only inside this repo."
//! watch-for = ["network egress to non-registry hosts"]
//!
//! [commands]
//! cargo = true            # blanket — key lexes to ["cargo"]
//! "git diff" = true       # sub-verb — key lexes to ["git", "diff"]
//! git = "clear"           # the stance vocabulary; `true` is sugar for this
//! "git push" = "deny"     # a carve-out: the longer key governs its subtree
//!
//! [paths]
//! "." = true            # sugar for { read = true, write = true }
//! "/refs" = { read = true }
//! ```
//!
//! ## What the config holds
//!
//! One config file configures every cascade rung (docs/permissions.md); the
//! [`Config`] fields below say what each holds. Per the convention in CLAUDE.md,
//! the schema fields land *before* the rungs that read them — types land ahead of
//! the feature that constructs them.
//!
//! ## Caching discipline
//!
//! The scope projects into the system preamble, which sits in the cached prefix,
//! so [`Config::preamble_section`] is **byte-stable for the whole session**
//! (docs/permissions.md, docs/log-and-projection.md): the config loads once at
//! startup, freezes, and renders purely from the scope text.
//!
//! This module is also the root of the config group, which holds configuration and what is
//! built from it: the permission policy (`policy`), the system preamble and its `[context]`
//! entries (`env`, `context`, `guide`), the `[env]` table (`env_table`), the recorded model
//! picks (`history`), the user configuration (`user_config`) and the published schemas
//! (`config_schemas`) (docs/README.md, *Dependency direction*).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value, value};

use crate::record::log::ConfigChange;

/// The shipped defaults, used when no `[bash]` config section says otherwise.
/// The asymmetry sets them: a premature detach costs one `read_process` and
/// loses nothing, an absent one costs the session.
pub const DEFAULT_BACKGROUND_AFTER: Duration = Duration::from_secs(300);
pub const DEFAULT_BACKGROUND_AFTER_MAX: Duration = Duration::from_secs(1800);

/// How long a foreground `bash` command may block the turn (docs/process-monitor.md).
///
/// Two numbers rather than one, because they answer different questions.
/// `background_after` is a *default* — what a call gets when it names no
/// expectation of its own. `background_after_max` is a *clamp* on the value a
/// call may name, and is the loop-safety property: no foreground call may park
/// the turn indefinitely. A single value could not be both, since clamping a
/// call to the default would only ever let it ask for less, and the point of the
/// call-supplied value is to let a genuinely slow build ask for more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashLimits {
    pub background_after: Duration,
    pub background_after_max: Duration,
}

impl Default for BashLimits {
    fn default() -> Self {
        Self {
            background_after: DEFAULT_BACKGROUND_AFTER,
            background_after_max: DEFAULT_BACKGROUND_AFTER_MAX,
        }
    }
}

/// How long one SSE response may stay open before the read loop stops waiting on
/// it (docs/transport.md). The retry policy bounds *failures*; this bounds
/// *success taking forever* — a stream that neither finalizes nor dies has no
/// other bound, and blocks its turn with nothing on `GET /state` to read.
///
/// One hour, set that far out deliberately: a ceiling that fires on a merely
/// *slow* stream discards real work, where one that fires late only delays the
/// diagnosis of a hang. Operators whose workload says otherwise set
/// `[transport] stream-ceiling` ([`TransportLimits`]).
pub const DEFAULT_STREAM_CEILING: Duration = Duration::from_secs(3600);

/// Transport-layer limits an operator can set (docs/transport.md). Both knobs
/// are operator-set because the right value is workload-dependent — a long think
/// and a hung gateway look identical from outside until one of them ends.
///
/// The two duration knobs are transport-only — nothing about them reaches the
/// request body, so no value of them can move the cached prefix
/// (docs/log-and-projection.md). `effort` is the exception: it *is* a request
/// field, read once at boot and frozen for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportLimits {
    /// How long a single response's SSE stream may stay open before the read
    /// loop stops waiting on it.
    pub stream_ceiling: Duration,
    /// How long the cached prefix may go untouched at idle before the keepalive
    /// pings it (docs/transport.md). `None` derives it from the provider's cache
    /// lifetime ([`crate::record::log::Provider::cache_ttl`]), so a set value is an override.
    /// **Zero turns the keepalive off** — the honest spelling for "do not ping",
    /// rather than a separate boolean that could disagree with it.
    pub keepalive_interval: Option<Duration>,
    /// How much effort the model spends per turn (docs/transport.md) — the level
    /// axis beside the thinking type. Held as the configured string verbatim:
    /// the vocabulary is the model's metadata, not a local enum, so a level the
    /// metadata invents is validated against that list at boot
    /// (`ModelCapabilities::check_effort`) and sent as spelled. `None` leaves
    /// the field off the request entirely, which is the provider's own default.
    /// `--effort` at creation overrides whatever this says; both are boot-time, and
    /// there is no runtime mutation path, because changing a thinking parameter
    /// mid-session invalidates the message-tier cache on Anthropic.
    pub effort: Option<String>,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            stream_ceiling: DEFAULT_STREAM_CEILING,
            keepalive_interval: None,
            effort: None,
        }
    }
}

// What a `[commands]` key says about the prefix it addresses (docs/permissions.md) — the
// spotter's stance vocabulary, spelled the same way at the fast path so one word means one
// thing at both rungs. A `bool` value is sugar: `true` = `Clear`, `false` = `Deny`. The variants'
// `///` docs are the published schema's per-value hover text (docs/config-schemas.md); the
// value's own hover text is `RawCommandValue`'s, so this type carries no `///`.
//
// Declaration order is the **restrictiveness order** (`Clear < Watch < Gate < Deny`), and
// the derived `Ord` breaks ties between keys of equal token depth toward the more
// restrictive one — reachable only through two spellings of the same prefix
// (`"git diff"` and `"git  diff"` lex alike).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CommandStance {
    /// Run without review.
    // Auto-approve at the fast path: no human, no spotter round-trip.
    Clear,
    /// Send to review, as `gate` does.
    // Distinct from `Gate` as a parsed value, identical in effect — no rung distinguishes
    // them (docs/permissions.md), the same collapse `Stance::Watch` has at the spotter rung.
    Watch,
    /// Send to review: the spotter, then the operator.
    Gate,
    /// Skip the spotter and go to the operator, who can still allow it; an autonomous
    /// session declines it.
    // The move skips the model rung entirely and goes to whoever gates (the human, or the
    // autonomous redirect). No stance below it, so nothing a spotter could say would change
    // the outcome — which is why the call is not made.
    Deny,
}

impl CommandStance {
    /// The token the config and `GET /state` spell it with.
    pub fn as_str(self) -> &'static str {
        match self {
            CommandStance::Clear => "clear",
            CommandStance::Watch => "watch",
            CommandStance::Gate => "gate",
            CommandStance::Deny => "deny",
        }
    }
}

/// The materialized policy table (docs/permissions.md). Built from the TOML
/// file by [`validate`](raw::validate)-ing a [`RawConfig`]; every field is optional/empty
/// so a minimal — or absent — file yields [`Config::default`]. This is what the cascade
/// consults; the config-edit fold mutates it by key
/// ([`crate::config::policy::apply_config_change`]).
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

    /// The kickoff: a user turn the harness submits when a session starts with an
    /// empty log, so the agent speaks first (docs/context.md). Part of the session's
    /// opening like the preamble — the clients do not render it as a turn — and
    /// read once at boot; editing it mid-session changes nothing. TOML:
    /// `first-turn = "…"`.
    pub first_turn: Option<String>,
    /// Opportunistic consolidation tool-text budget; absent uses 64 KiB (docs/consolidate.md).
    pub consolidation_bytes: Option<usize>,

    /// The bash prefix-allowlist as a **map** from command-prefix key to the [`CommandStance`]
    /// it carries (docs/permissions.md). `cargo` addresses the whole program, `git diff` only
    /// that sub-verb, and resolution is **longest-prefix-wins**, so a longer key carves its
    /// subtree out of a broader one (`git = "clear"`, `"git push" = "deny"`). Each key is
    /// guaranteed (at [`validate`](raw::validate) time) to lex to a single simple-command
    /// token vector; [`crate::config::policy`] re-lexes at match time to compare token-prefixes.
    /// TOML `[commands]` table; a `bool` value is sugar (`true` = `clear`, `false` = `deny`).
    pub commands: BTreeMap<String, CommandStance>,

    /// The `[tools]` table (docs/tools.md): which tools the session mounts
    /// ([`Config::tool_enabled`]) and which the fast-path clears by name
    /// ([`ToolsTable::granted`]). The path-bearing ones (`read`/`write`/`search`/edit)
    /// additionally require their target under a [`Config::paths`] grant; the daemon-internal
    /// ones clear on the name alone. Nothing is sanctioned in code — even `kill_process` is
    /// just an entry here. TOML: `[tools]` with `efference = { enabled = true, granted = true }`.
    pub tools: ToolsTable,

    /// Filesystem permission grants — the one value-bearing map (docs/permissions.md).
    /// Each key is a subtree, its [`Perms`] the read/write it grants; the fast-path clears a
    /// path-bearing tool only when its target resolves under a grant carrying the needed
    /// permission. cwd is not special in code — it is special only because the config says
    /// `"." = true`. TOML `[paths]` table; `"." = true` is sugar for `{ read = true, write = true }`.
    pub paths: BTreeMap<String, Perms>,

    /// Reserved ancestor pointers (tsconfig/eslint-style explicit inheritance). Parsed so a
    /// file can declare them; nothing reads them. TOML: `extends = ["base.toml"]`.
    pub extends: Vec<String>,

    /// The `[context]` preamble builder (docs/context.md): the markdown files and `exec`
    /// sections the operator makes available to the session, in **document order** — a `Vec`,
    /// not a sorted map, because the entries render as preamble sections in the order written.
    /// The harness bakes in nothing about what any entry "is" — see [`ContextEntry`].
    pub context: Vec<ContextEntry>,

    /// The `[env]` table as the operator wrote it (docs/env.md): per-variable
    /// `strip` (does a spawned child see it?) and `mask` (does the log ever store
    /// its value?). Held **unmerged** — the built-in defaults and the exact-beats-glob
    /// precedence are resolved by [`crate::config::env_table::Table`], so this map stays a
    /// faithful record of the file. Keys may be exact names or `*` globs.
    pub env: BTreeMap<String, env_table::EnvEntry>,

    /// Foreground-lifetime limits for the `bash` tool (docs/process-monitor.md).
    pub bash: BashLimits,
    /// Transport limits — the SSE stream duration ceiling
    /// (docs/transport.md). TOML `[transport]` table.
    pub transport: TransportLimits,
    /// User-declared MCP servers to mount as connected providers (docs/tools.md).
    /// A `BTreeMap` so mount order — and therefore the wire `tools[]` order — is
    /// deterministic (docs/log-and-projection.md). Declaring a server here is the
    /// human's authority; an agent edit to this table is gated like any other
    /// config edit. TOML `[mcp.servers.<name>]` tables.
    pub mcp_servers: BTreeMap<String, McpServerEntry>,
}

/// One `[mcp.servers.<name>]` stanza: how to reach the server, and whether the
/// session may start without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerEntry {
    pub transport: McpTransport,
    /// `optional = true` — start anyway if this server does not mount.
    ///
    /// Defaults to `false`, so a declared server that will not come up stops
    /// the boot. Declaring a server is the operator's statement that the
    /// session should have it (docs/tools.md); starting without it silently
    /// produces a session that is missing a capability nobody knows is
    /// missing, and an agent that answers "I can't do that" to a request the
    /// config says it can. Boot is also when a human is present to fix it.
    ///
    /// The flag is deliberately one boolean rather than a taxonomy of failure
    /// kinds: an uninstalled command, an unreachable host, a TLS problem, a
    /// server that needs authorization, and one that spews non-JSON on stdout
    /// are all "did not mount". Which of those it was belongs in the *message*,
    /// not in the config surface.
    pub optional: bool,
}

/// How to reach one declared MCP server ([`McpServerEntry`]). The shape of the
/// stanza picks the transport: `command` (+ optional `args`/`env`) spawns a
/// stdio subprocess; `url` connects over streamable HTTP. Exactly one of the two
/// is required — [`validate`](raw::validate) rejects a stanza with both or neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
    },
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
    /// Paths pruned from a directory's expansion, relative to each configured directory
    /// (`exclude = ["MEMORY.md", "archive"]` drops that file and that subtree). Matched
    /// component-wise during the walk, so an excluded subtree is never read — an unreadable
    /// file inside it cannot fail boot. Group-only; filters expansion, never an explicit path.
    pub exclude: Vec<String>,
    /// A prose hint for the model about how to use this entry's files — e.g. how to read or write
    /// a memory. Rendered inside the group's tag as a `<description>` element. Group-only; a bare
    /// path carrying `description` is rejected in
    /// [`validate_context_entry`](raw::validate_context_entry).
    pub description: Option<String>,
}

/// Where a [`ContextEntry`]'s bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSource {
    /// One or more file / dir / glob paths — a bare path (the table key) or a group's `path`/`paths`.
    Files(Vec<String>),
    /// A command whose stdout is the section body, run once at boot (docs/context.md).
    Exec(String),
    /// A set of documents compiled into the binary, listed as stubs at virtual paths
    /// (`{ builtin = "guide" }` — docs/context.md, [`crate::config::guide`]). The string is the
    /// mount name; `guide` is the only one, and [`validate_context_entry`] rejects any
    /// other rather than rendering an empty section.
    ///
    /// [`validate_context_entry`]: raw::validate_context_entry
    Builtin(String),
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
/// time (see [`crate::config::policy`]); both default `false`, so a bare `{ }` / `false` grants nothing.
/// Serialized as the `role` topic reports a grant (docs/control-api.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Perms {
    pub read: bool,
    pub write: bool,
}

/// The default config path, relative to cwd. Visible and committable (not under the
/// gitignored session dir) — a config is portable, shareable, not session state. The config
/// is *pointed-at*, not a magic scanned tree: only this one path is probed when `--config`
/// is absent.
pub const DEFAULT_PATH: &str = "efference.toml";

impl Config {
    /// Parse a config from TOML text — decode, then validate — labelling diagnostics
    /// with a stand-in path. The same path a file takes through `load`, for text that
    /// is not in a file: the test fixture builder (`config::testing`) reads the document
    /// it assembles through it, and a test whose subject is how a file is spelled passes
    /// its TOML here directly.
    pub fn from_toml(text: &str) -> Result<Self, LoadError> {
        parse(Path::new("<inline>"), text)
    }

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

/// Parse config text the way the load path does, without touching the filesystem — for tests
/// in other modules that need a [`Config`] as a real file would produce it (the `[commands]`
/// stance sugar, the path sugar) rather than one hand-constructed past validation.
#[cfg(test)]
pub(crate) fn parse_text(text: &str) -> Config {
    parse(Path::new("test.toml"), text).expect("test config parses")
}

/// [`parse_text`] keeping the error — for tests that assert a shape is *rejected*, and for
/// the guide's reference-validity check, which needs to name the document a bad block came
/// from rather than panicking with the parser's message alone.
#[cfg(test)]
pub(crate) fn try_parse_text(text: &str) -> Result<Config, LoadError> {
    parse(Path::new("test.toml"), text)
}

pub mod config_schemas;
pub mod context;
pub mod env;
pub mod env_table;
pub mod guide;
pub mod history;
pub mod policy;
pub mod user_config;

#[cfg(test)]
mod test;
#[cfg(test)]
pub(crate) mod testing;

mod edit;
mod file;
mod load;
mod raw;
mod tools;

pub use edit::diff_changes;
pub use file::{ConfigFile, Refreshed};
pub use load::{ConfigSource, LoadError, LoadedConfig, load, load_in};
pub(crate) use raw::RawConfig;
pub use raw::{ValidationError, wire_tool_name};
pub use tools::{BUILTIN_NAMESPACES, ToolSettings, ToolsTable, always_present, table_key};

use edit::apply_to_document;
use raw::parse;
