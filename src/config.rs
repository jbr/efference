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

    /// User-declared MCP servers to mount as connected providers (docs/tools.md).
    /// A `BTreeMap` so mount order — and therefore the wire `tools[]` order — is
    /// deterministic (docs/log-and-projection.md). Declaring a server here is the
    /// human's authority; an agent edit to this table is gated like any other
    /// config edit. TOML `[mcp.servers.<name>]` tables.
    pub mcp_servers: BTreeMap<String, McpTransport>,
}

/// How to reach one declared MCP server ([`Config::mcp_servers`]). The shape of
/// the stanza picks the transport: `command` (+ optional `args`/`env`) spawns a
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

/// The default config path, relative to cwd. Visible and committable (not under the
/// gitignored session dir) — a config is portable, shareable, not session state. The config
/// is *pointed-at*, not a magic scanned tree: only this one path is probed when `--config`
/// is absent.
pub const DEFAULT_PATH: &str = "efference.toml";

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

mod edit;
mod file;
mod load;
mod raw;

pub use edit::diff_changes;
pub use file::{ConfigFile, Refreshed};
pub use load::{ConfigSource, LoadError, LoadedConfig, load};
pub use raw::ValidationError;

use edit::apply_to_document;
use raw::parse;
