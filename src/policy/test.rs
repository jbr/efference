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

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: CallId("toolu_s".into()),
        name: name.into(),
        input,
    }
}
