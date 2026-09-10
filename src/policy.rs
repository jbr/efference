//! The permission fast-path — the deterministic, zero-model-cost rung of the cascade
//! (docs/permissions.md).
//!
//! [`classify`] is a **pure function** of `(cwd, role, tool call)` returning a [`Verdict`]:
//! `Clear` (auto-approve, no human, no model) or `Gate` (fall through to the spotter
//! rung, then the human gate). The classifier is the *mechanism*; the role config is the
//! *policy* — there are no special-cased program names or directories in here. It errs
//! toward `Gate`: any doubt — a parse failure, an operator, an expansion, an unmatched
//! prefix — gates, so being incomplete costs coverage and never soundness.
//!
//! Bash runs the two stages of docs/permissions.md in the order that doc fixes: stage 1
//! (structural simplicity, [`brush_parser`]) *before* stage 2 (prefix match,
//! [`commands::command_stance`]), so a trusted `git diff` prefix can never clear
//! `git diff > /etc/hosts`. The narrow shapes that survive a stage-1 rejection are
//! [`is_safe_fd_dup`], the leading `cd <dir> &&` ([`commands::bash_tokens_in`]), and the
//! redirection carve-out ([`commands::classify_redirect_write`]).
//!
//! Structured tools clear on a name grant ([`tool_granted`]) plus, where the tool names a
//! path, a `[paths]` grant carrying the needed permission. Mapping a tool to "which input
//! field holds the path, and is it a read or a write" means reaching into each built-in's
//! JSON input by known key — a wart with no way around it until an MCP superset can carry
//! permission metadata alongside the tool schema. The string literals are centralized in
//! [`path_check`] so the coupling is in one place.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::bash;
use crate::config::{CommandStance, Config, Perms};
use crate::log::{ConfigChange, LogEntry, Origin, Payload, Stance, ToolCall};

/// The fast-path's decision for one tool call. `Gate` is the extension point for the
/// spotter rung, which splits it into a graded `{clear, watch, gate}` stance
/// (docs/permissions.md, [`crate::spotter`]); `Deny` is the decision that skips that rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Certain-enough to auto-approve: no human, no model round-trip.
    Clear,
    /// Not cleared — fall through to the spotter rung, then the human gate.
    Gate,
    /// Refused by a `[commands]` key carrying [`CommandStance::Deny`]: the move goes to whoever
    /// gates (the human, or the autonomous redirect) **without** a spotter call. The spotter's
    /// most restrictive answer already resolves there, so there is no answer it could give that
    /// would change this outcome — the round-trip is spent for nothing (docs/permissions.md).
    Deny,
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
/// `path` grant its target needs. Resolves grants against the supplied session cwd,
/// the fixed root the built-in tools also resolve relative paths against.
pub fn classify(cwd: &Path, role: &Config, call: &ToolCall) -> Verdict {
    classify_in(role, call, cwd)
}

/// Stage 1's token vector for a bash command — the **bare** command, with a tolerated leading
/// `cd <granted dir> &&` stripped (docs/permissions.md). `None` if stage 1 rejects the command.
/// This is what prefix-promotion observes and probes, so a promoted prefix is the command itself
/// and generalizes across run directories rather than pinning one `cd` target. Resolves the `cd`
/// target against the supplied session cwd, like [`classify`].
pub fn bash_tokens(cwd: &Path, role: &Config, command: &str) -> Option<Vec<String>> {
    bash_tokens_in(role, command, cwd)
}

/// Whether `call` is a write whose target resolves (lexically, against the supplied session cwd) to
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
pub fn is_config_write(cwd: &Path, call: &ToolCall, protected: &Path) -> bool {
    is_config_write_in(call, protected, cwd)
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

/// The cwd-injected core, for deterministic tests. `bash` uses `cwd` to resolve the target of a
/// tolerated leading `cd` against the `[paths]` grants ([`commands::bash_tokens_in`]).
fn classify_in(role: &Config, call: &ToolCall, cwd: &Path) -> Verdict {
    if call.name == bash::NAME {
        match call.input.get("command").and_then(Value::as_str) {
            Some(command) => classify_bash(role, command, cwd),
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
        let cleared = tool_granted(role, &call.name)
            || role
                .context
                .iter()
                .any(|e| e.grants.update || e.grants.create);
        return verdict(cleared);
    }
    let Some((key, perm, default)) = path_spec(&call.name) else {
        // A structured tool with no path target (`kill_process`, `retarget_edit`, `consolidate`,
        // the supervisor/inbox tools): the name grant alone suffices.
        return verdict(tool_granted(role, &call.name));
    };
    let Some(targets) = input_targets(&call.input, key, default) else {
        return Verdict::Gate; // required path missing/empty/malformed → never auto-clear
    };
    // Per-path union of the two sources. `[context]` clears standalone; `[paths]` clears only when
    // the tool name is also granted (the name grant is a per-call precondition on that source).
    let name_granted = tool_granted(role, &call.name);
    let cleared = targets.iter().all(|t| {
        context_granted(role, t, perm, cwd) || (name_granted && path_granted(role, t, perm, cwd))
    });
    verdict(cleared)
}

/// Whether the config's `tools` set grants this wire tool name (docs/permissions.md).
///
/// Three spellings resolve to one decision, all of them set lookups — the grant set never
/// grows a pattern engine:
///
/// - the **wire name** itself (`ferritin__get`, `read`), which is what an exact grant stores
///   after `config::raw::wire_tool_name` has resolved whatever the operator wrote;
/// - the **namespace-qualified** name, so `efference.read` grants the built-in whose wire name
///   is the unprefixed `read` (for a connected tool this is the wire name again);
/// - the **whole-namespace glob** for the name's namespace ([`crate::mcp::namespace_glob`]).
///
/// The glob is a statement of trust in a *party* over a set nobody enumerated: `ferritin.*`
/// clears every tool that server advertises now and every one it adds later, with no spotter on
/// the way through, because the tool table is the server's to change. That is a different claim
/// from a blanket `[commands]` grant — a bash command still passes stage 1's structural check
/// first, and a structured call has no such pre-filter — which is why the glob is only ever an
/// explicit config act and never a default on mount.
pub fn tool_granted(role: &Config, wire_name: &str) -> bool {
    let namespace = crate::mcp::namespace_of(wire_name);
    role.tools.contains(wire_name)
        || role.tools.contains(&crate::mcp::namespace_glob(namespace))
        || (namespace == crate::mcp::BUILTIN_NAMESPACE
            && role.tools.contains(&format!(
                "{namespace}{}{wire_name}",
                crate::mcp::NAMESPACE_SEPARATOR
            )))
}

/// What a gated **structured** call is missing, phrased the way the config spells it
/// (docs/permissions.md). Consumed by the autonomous redirect, which names it back to the
/// model so a missing grant reads as a missing grant rather than as a broken tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingGrants {
    /// The wire tool name, when the `tools` set does not grant it; `None` when it does.
    pub tool: Option<String>,
    /// The call's targets that no `[paths]` or `[context]` grant covers with the needed
    /// permission, as the call spelled them.
    pub paths: Vec<String>,
    /// Whether those targets needed `write` (as against `read`) — which key of a `[paths]`
    /// entry would have to change.
    pub needs_write: bool,
}

/// Decompose why the fast-path did not clear a structured call into the grants that would
/// have (docs/permissions.md). `None` for `bash` (whose answer is a `[commands]` stance, not
/// a set of grants) and for a call the fast-path would clear.
///
/// The two halves are reported **independently**: the path check in [`classify_structured`]
/// requires the name grant as a precondition on the `[paths]` source, which would report
/// every target as ungranted whenever the tool name is missing. That is true of the
/// *decision* and misleading as a *diagnosis*, so the path half here asks only whether some
/// grant covers the target.
pub fn missing_grants(cwd: &Path, role: &Config, call: &ToolCall) -> Option<MissingGrants> {
    missing_grants_in(role, call, cwd)
}

/// The cwd-injected core of [`missing_grants`], for deterministic tests.
fn missing_grants_in(role: &Config, call: &ToolCall, cwd: &Path) -> Option<MissingGrants> {
    if call.name == bash::NAME {
        return None;
    }
    let tool = (!tool_granted(role, &call.name)).then(|| call.name.clone());
    let (paths, needs_write) = match path_spec(&call.name) {
        Some((key, perm, default)) => {
            let targets = input_targets(&call.input, key, default).unwrap_or_default();
            let ungranted = targets
                .into_iter()
                .filter(|t| {
                    !(context_granted(role, t, perm, cwd) || path_granted(role, t, perm, cwd))
                })
                .map(str::to_string)
                .collect::<Vec<_>>();
            (ungranted, perm == Perm::Write)
        }
        None => (Vec::new(), false),
    };
    (tool.is_some() || !paths.is_empty()).then_some(MissingGrants {
        tool,
        paths,
        needs_write,
    })
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
pub(crate) use commands::{bash_tokens_in, simple_command_tokens};
pub use effective::{apply_config_change, effective_config};
pub(crate) use paths::{absolutize, lexical_normalize, path_spec};
pub use probe::{ProbeLedger, probe_ledger};

use commands::classify_bash;
use paths::*;
