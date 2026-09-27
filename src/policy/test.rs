use super::*;

/// The review-level dial (docs/permissions.md) round-trips through its
/// `u8` ordinal (atomic storage) and its wire token (`/state`, `/review-level`, CLI), and both
/// decoders fail *safe* — an unknown ordinal / token never silently becomes `Autonomous`.
#[test]
fn review_level_round_trips_and_fails_safe() {
    for level in [
        ReviewLevel::Autonomous,
        ReviewLevel::Upstream,
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
    // The gear order, pinned: ordered by how much a position costs a human, which an
    // upstream agent reduces without eliminating.
    assert_eq!(
        [
            ReviewLevel::Autonomous.as_u8(),
            ReviewLevel::Upstream.as_u8(),
            ReviewLevel::SpotterHuman.as_u8(),
            ReviewLevel::HumanOnly.as_u8(),
        ],
        [0, 1, 2, 3]
    );
    assert_eq!(
        [
            ReviewLevel::Autonomous.as_str(),
            ReviewLevel::Upstream.as_str(),
            ReviewLevel::SpotterHuman.as_str(),
            ReviewLevel::HumanOnly.as_str(),
        ],
        ["autonomous", "upstream", "spotter_human", "human_only"]
    );
    // Fail-safe: an out-of-range ordinal or unknown token defaults to supervised, never
    // silently to the unsupervised `Autonomous`.
    assert_eq!(ReviewLevel::from_u8(99), ReviewLevel::SpotterHuman);
    assert_eq!(ReviewLevel::parse("nonsense"), None);
}

/// Parse a config from TOML for a test that needs one as an *input* — the operator's own
/// vocabulary, checked by the real parser rather than assembled field by field.
fn role(toml: &str) -> Config {
    Config::from_toml(toml).expect("valid config")
}

/// A config whose command-allowlist mirrors the curation rule: blanket for whole-arg-space-
/// recoverable programs (`cargo`, `rg`), sub-verb for the irreversible frontier
/// (`git diff`/`status`, never `git push`).
fn sample_role() -> Config {
    role(
        r#"
        [commands]
        cargo = true
        rg = true
        "git diff" = true
        "git status" = true
        "#,
    )
}

fn bash_call(command: &str) -> ToolCall {
    ToolCall::new(
        bash::NAME,
        "toolu_x",
        serde_json::json!({ "command": command }),
    )
}

fn verdict(command: &str) -> Verdict {
    classify(Path::new(CWD), &sample_role(), &bash_call(command))
}

// --- Stage 1: structural simplicity (decided before any prefix match) ---

// --- Stage 2: prefix match, only reached for structurally-simple commands ---

// --- Structured tools: name grant + path-perm, interpreted from config ---

use crate::config::Perms;

/// A fixed cwd so path resolution is deterministic in tests (no real filesystem).
const CWD: &str = "/work";

// --- effective_config: the policy fold over the log's changes ---

use crate::log::{ConfigChange, ConfigEdit, EditState, LogEntry, Payload, Timestamp};

// --- prefix-promotion: the probe ledger + descent (docs/permissions.md) ---

// --- the [context] clearance rung (docs/context.md) ---

mod commands;
mod context_rung;
mod effective;
mod probe;
mod tools;

/// A config granting the read/write/daemon tools, with cwd read+write and a **disjoint**
/// read-only zone outside cwd (`/refs`). Disjoint on purpose: grants are additive with
/// no sub-deny, so a read-only grant can only restrict where the writable cwd grant
/// doesn't already reach (see [`path_granted`]).
fn fs_role() -> Config {
    role(
        r#"
        tools = ["read", "list", "search", "write", "edit", "undo_edit", "kill_process"]

        [paths]
        "." = true
        "/refs" = { read = true }
        "#,
    )
}

fn structured(role: &Config, c: &ToolCall) -> Verdict {
    classify_in(role, c, Path::new(CWD))
}

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall::new(name, "toolu_s", input)
}

/// Which leading `cd`s go nowhere: into the working directory by any spelling that
/// resolves there, and nothing else (docs/permissions.md).
#[test]
fn a_cd_is_dropped_only_when_it_goes_nowhere() {
    let cwd = std::path::Path::new("/work/proj");
    let drop = |command| without_cd_into(command, cwd);
    assert_eq!(drop("cd /work/proj && ls"), Some("ls"));
    assert_eq!(drop("cd . && ls -a"), Some("ls -a"));
    assert_eq!(drop("cd ../proj && ls"), Some("ls"));
    assert_eq!(drop("cd .. && ls"), None);
    assert_eq!(drop("cd /work/other && ls"), None);
    assert_eq!(drop("cd /work/proj"), None);
    assert_eq!(drop("cd /work/proj && "), None);
    assert_eq!(drop("pushd /work/proj && ls"), None);
}
