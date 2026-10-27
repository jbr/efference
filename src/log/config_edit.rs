//! One change to the session's policy table (docs/permissions.md).

use super::*;

/// One config edit: a single, durable change to the policy table (docs/permissions.md).
/// The change vocabulary lives here, in the log types, deliberately — like [`Stance`] and
/// [`Origin`], the log owns its own serialized types so the wire format isn't hostage to a
/// config struct's layout. [`crate::policy::apply_config_change`] applies each [`ConfigChange`] to
/// the config it folds into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEdit {
    /// Where this edit sits in the propose→commit/cancel lifecycle
    /// (docs/permissions.md). The fold gates on it: only [`EditState::Committed`]
    /// takes effect (and, for session-visible keys, notifies); a [`EditState::Proposed`] entry is
    /// inert in the effective config — it sits in the daemon's pending-proposals queue awaiting an
    /// orchestrator's commit. The synchronous human-gate path appends `Committed` directly.
    pub state: EditState,
    /// Which pending proposal this entry resolves, by the `Proposed` entry's `seq` — the foreign
    /// key from a `Committed`/`Cancelled` decision back to its proposal (the audit link, and how
    /// the daemon drops the right entry from its pending-proposals queue). `None` for a *direct*
    /// commit that had no prior proposal (the synchronous human gate, fs-watch) and for a
    /// `Proposed` entry itself (a proposal resolves nothing).
    pub resolves: Option<Seq>,
    /// What changed: which slot, and its new value. Replace-at-key — the fold sets the addressed
    /// key, last-write-wins, so replaying a log in order materializes the table deterministically.
    pub change: ConfigChange,
    /// The orchestrator's one-line rationale, addressed to the session (and the corpus byproduct:
    /// the labeled `(change, why)` pair for any future triaged approval model).
    pub note: String,
}

/// Where a [`ConfigEdit`] sits in the propose→commit/cancel lifecycle
/// (docs/permissions.md). Append-only means the three states are **distinct
/// entries sharing the proposal's `seq`**, not a mutated record: an async initiator (the session's
/// `propose_scope`, the spotter's prefix-promotion) appends a `Proposed`, and the orchestrator later
/// appends a `Committed` or `Cancelled`. The synchronous human-gate path and fs-watch append
/// `Committed` directly (the human's gesture / file edit *is* the commit). The fold applies only
/// `Committed`; `Proposed` and `Cancelled` are audit-only and never reach the effective config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditState {
    /// Filed by an async initiator, awaiting an orchestrator decision. Inert in the effective
    /// config; lives in the daemon's pending-proposals queue, addressed by its `seq`.
    Proposed,
    /// In effect: the fold applies its `change` (and, for `scope`/`paths`, notifies the session).
    Committed,
    /// The orchestrator declined a `Proposed` edit. Audit-only — drops the proposal from the queue,
    /// never touches the effective config.
    Cancelled,
}

/// What a [`ConfigEdit`] changes — the `{ which slot, new value }` reshape
/// (docs/permissions.md). Each variant addresses one section of the config table;
/// [`crate::policy::apply_config_change`] sets the named key (replace-at-key, last-write-wins).
/// Most variants set membership or a perm; [`ConfigChange::Scope`] replaces a scalar — the
/// narrowing edit append-only KDL couldn't do. The `watch-for` slot is unbuilt (its replace
/// semantics over a list are an unsettled question; add it when an initiator needs it).
///
/// **Session-visibility** (which edits notify the session) is decided by variant in
/// [`crate::daemon`]: `Scope` and `Path` are the perimeter the session should learn moved;
/// `Command` and `Tool` reach the model only through the classifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigChange {
    /// Clear a bash command-prefix: add `command` to the `[commands]` set. The string is the
    /// key itself (a re-lexable command prefix); empty-args = blanket-trust the program.
    Command { command: String },
    /// Set a filesystem subtree's perms: `path` and everything beneath it (`write` implies
    /// `read`, as in the config). **Session-visible** — moving the spatial perimeter notifies.
    Path {
        path: String,
        read: bool,
        write: bool,
    },
    /// Grant a structured tool: set `granted` on its `[tools]` entry. `name` is the tool's
    /// wire name, or a whole namespace as `<namespace>__*` ([`crate::mcp::namespace_glob`]).
    Tool { name: String },
    /// Replace the session **scope** (the perimeter prose). **Session-visible** — the boot
    /// preamble stays frozen (re-rendering it would mutate behind the cache breakpoint), so a
    /// live scope edit reaches the session as a notification, not a preamble rewrite
    /// (docs/permissions.md).
    Scope { text: String },
}
