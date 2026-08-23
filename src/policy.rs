//! The permission fast-path — the deterministic, zero-model-cost rung of the cascade
//! (docs/permissions.md, docs/permissions.md).
//!
//! [`classify`] is a **pure function** of `(role, tool call)` returning a [`Verdict`]:
//! `Clear` (auto-approve, no human, no model) or `Gate` (fall through to the spotter
//! rung, then the human gate). The classifier is the *mechanism*; the role config is the
//! *policy* — there are no special-cased program names or directories in here.
//!
//! ## Why this rung is sound despite being incomplete
//!
//! The threat model is **error, not deception** (docs/permissions.md): a
//! well-intentioned session reaching for the wrong tool, not an adversary obfuscating
//! bash. So the fast-path only needs to *accept what it is certain about* and pass
//! everything else down. It classifies **structural simplicity** (decidable) rather than
//! **behavior** (not), and it errs toward `Gate`: any doubt — a parse failure, an
//! operator, an expansion, an unmatched prefix — gates.
//!
//! ## Bash: the two-stage check (the order matters)
//!
//! Stage 1 (structural simplicity) runs *before* stage 2 (prefix match), so a trusted
//! `git diff` prefix can never clear `git diff > /etc/hosts` or `git diff $(rm x)` —
//! those die in stage 1, before any prefix is consulted.
//!
//! - **Stage 1** parses the command with [`yash_syntax`] (a real POSIX parser, not a
//!   regex) and accepts only a *single simple command* — no operators (`| & ; && ||`),
//!   no *file* redirection, no command/arithmetic substitution, no parameter/tilde expansion,
//!   no env-assignment prefix, no unquoted glob/brace. The one redirection it tolerates is a
//!   pure file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`) — it rewires the process's
//!   own fds and touches no filesystem, so it's sound and lets the habitual `cmd 2>&1` stay on the
//!   fast path ([`is_safe_fd_dup`]). yash is POSIX-strict, which only
//!   costs *coverage* (a few bash-only forms gate to the spotter), never *soundness* —
//!   bash's dangerous operators are POSIX too, so they parse as operators and gate;
//!   bash-only syntax either fails to parse (→ gate) or surfaces as inert literal words
//!   we still filter. yash parses (does not expand), so `git diff HEAD~1` is fine — we
//!   do the expansion-metacharacter filtering ourselves.
//! - **Stage 2** matches the lexed `[program, args…]` against the role's
//!   `(program, safe-arg-prefix)` allowlist (token-wise prefix; empty prefix = blanket).
//!
//! ## Structured tools
//!
//! A structured tool clears only if its name is in the role's `tool` grants **and**, for
//! the path-bearing ones, its target resolves under a `path` grant carrying the needed
//! permission (reads need `read`, writes need `write`; `write` implies `read`). Nothing
//! clears unless the config says so — an empty role gates everything, exactly as before.
//!
//! Mapping a tool to "which input field holds the path, and is it a read or a write"
//! requires reaching into each built-in's JSON input by known key ([`path_check`]). That
//! is a deliberate wart: there is no way around it until an MCP-superset can carry
//! permission metadata alongside the tool schema. The string literals are centralized in
//! [`path_check`] so the coupling is in one place. One subtlety it encodes: `undo_edit`
//! writes to disk but carries *no* path (the target was named by an earlier
//! `edit`), so write permission is enforced at `edit` — the only
//! edit-pipeline call that names the file — and `undo_edit` clears on the name grant.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::bash;
use crate::config::{Config, Perms};
use crate::log::{ConfigChange, LogEntry, Origin, Payload, Stance, ToolCall};

/// The fast-path's decision for one tool call. Binary; the extension point for the
/// spotter rung, which splits `Gate` into a graded `{clear, watch, gate}` stance
/// (docs/permissions.md, [`crate::spotter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Certain-enough to auto-approve: no human, no model round-trip.
    Clear,
    /// Not cleared — fall through to the spotter rung, then the human gate.
    Gate,
}

/// The operator's **review level** (docs/permissions.md): who reviews a
/// call the deterministic fast-path didn't clear. An ordinal dial of increasing human involvement,
/// runtime-settable over the control channel (unlike the frozen base config). Orthogonal to pace —
/// any combination is meaningful (autonomous-but-slow, human-only-and-held). Pause is *not* a
/// position here; that's the `held` overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewLevel {
    /// No synchronous human: a spotter `Watch`/`Gate` redirects the model to its config
    /// (`agent::autonomous_gate_message`) instead of blocking on an absent gate.
    Autonomous,
    /// The supervised default: fast-path → spotter → human gate.
    SpotterHuman,
    /// The human takes the spotter's role **entirely** — skip the spotter (pay no tokens, log no
    /// `Spot`); every call the fast-path didn't clear goes straight to the human, who both gates it
    /// and grows the allowlist by hand (the `approve-always-at-prefix` grant).
    HumanOnly,
}

impl ReviewLevel {
    /// Ordinal for lock-free `AtomicU8` storage; also the gear position a UI can present.
    pub fn as_u8(self) -> u8 {
        match self {
            ReviewLevel::Autonomous => 0,
            ReviewLevel::SpotterHuman => 1,
            ReviewLevel::HumanOnly => 2,
        }
    }

    /// Inverse of [`as_u8`](Self::as_u8); any out-of-range value falls back to the supervised
    /// default (fail-safe: an unknown ordinal never silently drops to autonomous).
    pub fn from_u8(n: u8) -> Self {
        match n {
            0 => ReviewLevel::Autonomous,
            2 => ReviewLevel::HumanOnly,
            _ => ReviewLevel::SpotterHuman,
        }
    }

    /// The wire / CLI token (snake_case), for `GET /state`, `PATCH /loop {review_level}`, and `efference
    /// review-level`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewLevel::Autonomous => "autonomous",
            ReviewLevel::SpotterHuman => "spotter_human",
            ReviewLevel::HumanOnly => "human_only",
        }
    }

    /// Parse the wire / CLI token; `None` if unrecognized (the caller reports a 400 / usage error
    /// rather than guessing).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "autonomous" => Some(ReviewLevel::Autonomous),
            "spotter_human" => Some(ReviewLevel::SpotterHuman),
            "human_only" => Some(ReviewLevel::HumanOnly),
            _ => None,
        }
    }
}

/// A filesystem permission a path-bearing tool needs to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Perm {
    Read,
    Write,
}

/// How many sightings of a command-prefix before a prefix-promotion probe rides along — the
/// frequency gate (docs/permissions.md). `2` = probe on the *second* time we see
/// the prefix; "we don't propose promoting a shape we've seen only once." Tuneable — the decay curve
/// sets the real value.
pub const MIN_SIGHTINGS_TO_PROBE: u32 = 2;

/// How many independent safe-votes promote a prefix — the consensus threshold `K`. `1` = the first
/// `clear_always` files a proposal for an orchestrator to confirm. Raising it (and tolerating a few
/// dissenting not-safe votes) is the path to evidence-only, human-out-of-the-loop promotion; the
/// vote ledger already records the evidence, so it's a threshold change, not a schema one.
pub const PROMOTE_AFTER_SAFE_VOTES: u32 = 1;

/// Classify a tool call against the role (docs/permissions.md).
/// `bash` runs the two-stage check; a structured tool clears on its `tool` grant plus any
/// `path` grant its target needs. Resolves grants against the process cwd (session-stable,
/// the fixed root the built-in tools also resolve relative paths against).
pub fn classify(role: &Config, call: &ToolCall) -> Verdict {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    classify_in(role, call, &cwd)
}

/// Whether `call` is a write whose target resolves (lexically, against the process cwd) to
/// `protected` — the session's own config file. A write to the policy file is a **proposal** that
/// must always reach the operator, so the loop forces the human gate on it, **bypassing both the
/// fast-path and the spotter** — neither may auto-clear a permission change
/// (docs/permissions.md). This is the one named-meta-file
/// exception to "no deny carve-out inside an allowed subtree": `.` may grant the tree, but this file
/// still gates.
///
/// Only the write-class tools that *name* a path qualify (`write`→`path`, `edit`→`file_path`).
/// `undo_edit` carries no path, but it can only revert a previously-`edit`ed target — and
/// an `edit` of the config is caught here — so the review point is the naming step, exactly
/// where write permission is already enforced (the acknowledged per-tool wart in `path_check`).
pub fn is_config_write(call: &ToolCall, protected: &Path) -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    is_config_write_in(call, protected, &cwd)
}

/// The cwd-injected core of [`is_config_write`], for deterministic tests.
fn is_config_write_in(call: &ToolCall, protected: &Path, cwd: &Path) -> bool {
    let key = match call.name.as_str() {
        "write" => "path",
        "edit" => "file_path",
        _ => return false,
    };
    let Some(target) = call.input.get(key).and_then(Value::as_str) else {
        return false;
    };
    let Some(protected) = protected.to_str() else {
        return false;
    };
    lexical_normalize(&absolutize(target, cwd)) == lexical_normalize(&absolutize(protected, cwd))
}

/// The cwd-injected core, for deterministic tests. `bash` ignores `cwd`.
fn classify_in(role: &Config, call: &ToolCall, cwd: &Path) -> Verdict {
    if call.name == bash::NAME {
        match call.input.get("command").and_then(Value::as_str) {
            Some(command) => classify_bash(role, command),
            // A bash call without a string `command` is malformed — let the gate see it.
            None => Verdict::Gate,
        }
    } else {
        classify_structured(role, call, cwd)
    }
}

/// A structured (non-bash) tool clears when every path it targets is granted — by **either**
/// clearance source, decided **per path**: a `[paths]` grant (which requires the tool name to be in
/// `tools`) *or* a `[context]` listing (a standalone grant needing no name — docs/context.md). The
/// union is per-path on purpose: a batched `read` mixing a `[paths]`-covered file and a
/// `[context]`-covered one clears, because each path is authorized by *some* source, even though
/// neither source covers the whole batch alone (the dogfood regression). A path-less tool
/// (`kill_process`, …) clears on the name grant; `undo_edit` also clears on any `[context]` write
/// grant. Anything not granted gates.
fn classify_structured(role: &Config, call: &ToolCall, cwd: &Path) -> Verdict {
    // `undo_edit` names no path (its target was named by the `edit` where write is enforced — the
    // acknowledged wart in [`path_spec`]). It clears on the name grant, or on any `[context]` write
    // grant, mirroring its clear-on-name rule under `tools`.
    if call.name == "undo_edit" {
        let cleared = role.tools.contains(&call.name)
            || role
                .context
                .iter()
                .any(|e| e.grants.update || e.grants.create);
        return verdict(cleared);
    }
    let Some((key, perm, default)) = path_spec(&call.name) else {
        // A structured tool with no path target (`kill_process`, `retarget_edit`, `consolidate`,
        // the supervisor/inbox tools): the name grant alone suffices.
        return verdict(role.tools.contains(&call.name));
    };
    let Some(targets) = input_targets(&call.input, key, default) else {
        return Verdict::Gate; // required path missing/empty/malformed → never auto-clear
    };
    // Per-path union of the two sources. `[context]` clears standalone; `[paths]` clears only when
    // the tool name is also granted (the name grant is a per-call precondition on that source).
    let name_granted = role.tools.contains(&call.name);
    let cleared = targets.iter().all(|t| {
        context_granted(role, t, perm, cwd) || (name_granted && path_granted(role, t, perm, cwd))
    });
    verdict(cleared)
}

/// Tiny helper: fold a boolean clearance decision into a [`Verdict`].
fn verdict(cleared: bool) -> Verdict {
    if cleared {
        Verdict::Clear
    } else {
        Verdict::Gate
    }
}

#[cfg(test)]
mod test;

mod commands;
mod effective;
mod paths;
mod probe;

pub use commands::promotable_command;
pub(crate) use commands::simple_command_tokens;
pub use effective::{apply_config_change, effective_config};
pub(crate) use paths::{absolutize, lexical_normalize, path_spec};
pub use probe::{ProbeLedger, probe_ledger};

use commands::classify_bash;
use paths::*;
