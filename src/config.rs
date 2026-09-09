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
//! One artifact configures every cascade rung (docs/permissions.md):
//!
//! - [`Config::scope`] — the positive scope, **shared with the acting session** for
//!   self-alignment. It projects into the cached preamble ([`Config::preamble_section`]).
//! - [`Config::watch_for`] — the spotter's "things to watch for", **spotter-only**.
//!   Kept off the session's preamble for attention-division (the focused session
//!   doesn't carry peripheral hazards; the spotter does). Consumed by the spotter
//!   rung ([`crate::spotter`]).
//! - [`Config::commands`] — the bash prefix-allowlist the deterministic fast-path matches
//!   against (docs/permissions.md). A **map** from command-prefix key to [`CommandStance`],
//!   resolved longest-prefix-wins. Consumed by that rung.
//! - [`Config::tools`] — structured tools the fast-path clears by name (also a set).
//! - [`Config::paths`] — filesystem subtree grants, the one genuine value-bearing map.
//! - [`Config::trust_threshold`] — where the watch-window/gate cutoffs sit. The
//!   watch-window rung is its only reader; the field lands ahead of it, per below.
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
use serde::{Deserialize, Serialize};
use std::time::Duration;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value, value};

use crate::log::ConfigChange;

/// How long a foreground `bash` command may block the turn (docs/supervisor.md).
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
            background_after: crate::bash::DEFAULT_BACKGROUND_AFTER,
            background_after_max: crate::bash::DEFAULT_BACKGROUND_AFTER_MAX,
        }
    }
}

/// Transport-layer limits an operator can set (docs/transport.md). Both knobs
/// are operator-set because the right value is workload-dependent — a long think
/// and a hung gateway look identical from outside until one of them ends, and
/// how long a provider's prompt cache lives is a provider fact the harness does
/// not read anywhere.
///
/// The two duration knobs are transport-only — nothing about them reaches the
/// request body, so no value of them can move the cached prefix
/// (docs/log-and-projection.md). `effort` is the exception: it *is* a request
/// field. It is read once at boot and frozen for the session, so it moves the
/// prefix no more than `model` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportLimits {
    /// How long a single response's SSE stream may stay open before the read
    /// loop stops waiting on it.
    pub stream_ceiling: Duration,
    /// How long the cached prefix may go untouched at idle before the keepalive
    /// pings it (docs/transport.md). The default sits under Anthropic's
    /// five-minute ephemeral TTL; a provider with a shorter one needs a shorter
    /// interval, and one with a longer one is being pinged more often than it
    /// needs. **Zero turns the keepalive off** — the honest spelling for "do not
    /// ping", rather than a separate boolean that could disagree with it.
    pub keepalive_interval: Duration,
    /// How much effort the model spends per turn (docs/transport.md) — the level
    /// axis beside the thinking type. Held as the configured string verbatim:
    /// the vocabulary is the model's metadata, not a local enum, so a level the
    /// metadata invents is validated against that list at boot
    /// (`ModelCapabilities::check_effort`) and sent as spelled. `None` leaves
    /// the field off the request entirely, which is the provider's own default.
    /// `serve --effort` overrides whatever this says; both are boot-time, and
    /// there is no runtime mutation path, because changing a thinking parameter
    /// mid-session invalidates the message-tier cache on Anthropic.
    pub effort: Option<String>,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            stream_ceiling: crate::api::DEFAULT_STREAM_CEILING,
            keepalive_interval: crate::daemon::DEFAULT_KEEPALIVE_INTERVAL,
            effort: None,
        }
    }
}

/// What a `[commands]` key says about the prefix it addresses (docs/permissions.md) — the
/// spotter's stance vocabulary, spelled the same way at the fast path so one word means one
/// thing at both rungs. A `bool` value is sugar: `true` = [`Clear`], `false` = [`Deny`].
///
/// The declaration order is the **restrictiveness order** (`Clear < Watch < Gate < Deny`), and
/// the derived `Ord` is what breaks a tie between two keys of equal token depth: the more
/// restrictive one governs. Equal depth is only reachable through two spellings of the same
/// prefix (`"git diff"` and `"git  diff"` lex alike), so the tie-break decides a case that is
/// otherwise a coin flip, in the safe direction.
///
/// [`Clear`]: CommandStance::Clear
/// [`Deny`]: CommandStance::Deny
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandStance {
    /// Auto-approve at the fast path: no human, no spotter round-trip.
    Clear,
    /// Send the move to the rung that gates it. Distinct from [`CommandStance::Gate`] as a
    /// parsed value, identical in effect until the watch-window lands and gives `watch` its own
    /// intervention latency (docs/permissions.md) — the same collapse `Stance::Watch` has at the
    /// spotter rung.
    Watch,
    /// Send the move to the rung that gates it — the spotter, then the human.
    Gate,
    /// Refuse without consulting the spotter: the move skips the model rung entirely and goes
    /// to whoever gates (the human, or the autonomous redirect). No stance below it, so nothing
    /// a spotter could say would change the outcome — which is why the call is not made.
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

    /// The bash prefix-allowlist as a **map** from command-prefix key to the [`CommandStance`]
    /// it carries (docs/permissions.md). `cargo` addresses the whole program, `git diff` only
    /// that sub-verb, and resolution is **longest-prefix-wins**, so a longer key carves its
    /// subtree out of a broader one (`git = "clear"`, `"git push" = "deny"`). Each key is
    /// guaranteed (at [`validate`] time) to lex to a single simple-command token vector;
    /// [`crate::policy`] re-lexes at match time to compare token-prefixes. TOML `[commands]`
    /// table; a `bool` value is sugar (`true` = `clear`, `false` = `deny`).
    pub commands: BTreeMap<String, CommandStance>,

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

    /// The `[env]` table as the operator wrote it (docs/env.md): per-variable
    /// `strip` (does a spawned child see it?) and `mask` (does the log ever store
    /// its value?). Held **unmerged** — the built-in defaults and the exact-beats-glob
    /// precedence are resolved by [`crate::env_table::Table`], so this map stays a
    /// faithful record of the file. Keys may be exact names or `*` globs.
    pub env: BTreeMap<String, crate::env_table::EnvEntry>,

    /// Foreground-lifetime limits for the `bash` tool (docs/supervisor.md).
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
/// is required — [`validate`] rejects a stanza with both or neither.
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
    /// path carrying `description` is rejected in [`validate_context_entry`].
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
    /// (`{ builtin = "guide" }` — docs/context.md, [`crate::guide`]). The string is the
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
/// time (see [`crate::policy`]); both default `false`, so a bare `{ }` / `false` grants nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// with a stand-in path. The same path a file takes through `load`, exposed so a
    /// config used as an *input* (a policy or spotter test, a scripted scenario) is
    /// written in the vocabulary an operator writes and checked by the parser that
    /// reads theirs, rather than assembled field by field around it.
    pub fn from_toml(text: &str) -> Result<Self, LoadError> {
        raw::parse(Path::new("<inline>"), text)
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

#[cfg(test)]
mod test;

mod edit;
mod file;
mod load;
mod raw;

pub use edit::diff_changes;
pub use file::{ConfigFile, Refreshed};
pub use load::{ConfigSource, LoadError, LoadedConfig, load};
pub use raw::{ValidationError, wire_tool_name};

use edit::apply_to_document;
use raw::parse;
