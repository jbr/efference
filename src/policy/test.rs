use super::*;
use crate::config::testing::ConfigFixture;

/// The review-level dial (docs/permissions.md) round-trips through its
/// `u8` ordinal (atomic storage) and its wire token (`/state`, `/review-level`, CLI), and both
/// decoders fail *safe* — an unknown ordinal / token never silently becomes `Autonomous`.
#[test]
fn review_level_round_trips_and_fails_safe() {
    for level in [
        ReviewLevel::Autonomous,
        ReviewLevel::Supervisor,
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
    // supervising agent reduces without eliminating.
    assert_eq!(
        [
            ReviewLevel::Autonomous.as_u8(),
            ReviewLevel::Supervisor.as_u8(),
            ReviewLevel::SpotterHuman.as_u8(),
            ReviewLevel::HumanOnly.as_u8(),
        ],
        [0, 1, 2, 3]
    );
    assert_eq!(
        [
            ReviewLevel::Autonomous.as_str(),
            ReviewLevel::Supervisor.as_str(),
            ReviewLevel::SpotterHuman.as_str(),
            ReviewLevel::HumanOnly.as_str(),
        ],
        ["autonomous", "supervisor", "spotter_human", "human_only"]
    );
    // Fail-safe: an out-of-range ordinal or unknown token defaults to supervised, never
    // silently to the unsupervised `Autonomous`.
    assert_eq!(ReviewLevel::from_u8(99), ReviewLevel::SpotterHuman);
    assert_eq!(ReviewLevel::parse("nonsense"), None);
}

/// Parse a config from TOML, for an input holding entries [`ConfigFixture`] does not
/// write (a `[context]` listing).
fn role(toml: &str) -> Config {
    Config::from_toml(toml).expect("valid config")
}

/// A config whose command-allowlist mirrors the curation rule: blanket for whole-arg-space-
/// recoverable programs (`cargo`, `rg`), sub-verb for the irreversible frontier
/// (`git diff`/`status`, never `git push`).
fn sample_role() -> Config {
    ConfigFixture::new()
        .with_command("cargo", CommandStance::Clear)
        .with_command("rg", CommandStance::Clear)
        .with_command("git diff", CommandStance::Clear)
        .with_command("git status", CommandStance::Clear)
        .build()
}

fn bash_call(command: &str) -> ToolCall {
    ToolCall::new(
        bash::NAME,
        "toolu_x",
        serde_json::json!({ "command": command }),
    )
}

fn verdict(command: &str) -> Verdict {
    classify(
        &Directories::at(Path::new(CWD)),
        &sample_role(),
        &bash_call(command),
    )
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
mod gate_grant;
mod probe;
mod tools;

/// A config granting the read/write/daemon tools, with cwd read+write and a **disjoint**
/// read-only zone outside cwd (`/refs`). Disjoint on purpose: grants are additive with
/// no sub-deny, so a read-only grant can only restrict where the writable cwd grant
/// doesn't already reach (see [`path_granted`]).
fn fs_role() -> Config {
    ConfigFixture::new()
        .with_tool("read")
        .with_tool("list")
        .with_tool("search")
        .with_tool("write")
        .with_tool("edit")
        .with_tool("undo_edit")
        .with_tool("kill_process")
        .with_path(".", true, true)
        .with_path("/refs", true, false)
        .build()
}

fn structured(role: &Config, c: &ToolCall) -> Verdict {
    classify_in(role, c, &Directories::at(Path::new(CWD)))
}

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall::new(name, "toolu_s", input)
}

/// Which leading `cd`s go nowhere: into the working directory by any spelling that
/// resolves there, and nothing else (docs/permissions.md).
#[test]
fn a_cd_is_dropped_only_when_it_goes_nowhere() {
    let cwd = Path::new("/work/proj");
    let drop = |command| without_cd_into(command, &Directories::at(cwd));
    assert_eq!(drop("cd /work/proj && ls"), Some("ls"));
    assert_eq!(drop("cd . && ls -a"), Some("ls -a"));
    assert_eq!(drop("cd ../proj && ls"), Some("ls"));
    assert_eq!(drop("cd .. && ls"), None);
    assert_eq!(drop("cd /work/other && ls"), None);
    assert_eq!(drop("cd /work/proj"), None);
    assert_eq!(drop("cd /work/proj && "), None);
    assert_eq!(drop("pushd /work/proj && ls"), None);
}

// --- Directories: grants resolve against the project, targets against the working dir ---

fn path_call(name: &str, path: &str) -> ToolCall {
    ToolCall::new(name, "toolu_d", serde_json::json!({ "path": path }))
}

fn worktree_session() -> Directories {
    Directories::new(
        PathBuf::from("/p"),
        PathBuf::from("/p/.efference/worktrees/w"),
    )
}

/// A relative write from a worktree maps to the project path, so the project's `.` grant
/// clears it (docs/permissions.md).
#[test]
fn a_worktree_write_clears_under_the_project_grant() {
    let role = ConfigFixture::new()
        .with_tool("write")
        .with_path(".", false, true)
        .build();
    let call = path_call("write", "src/x.rs");
    assert_eq!(classify(&worktree_session(), &role, &call), Verdict::Clear);
}

/// A narrower project-level deny governs the same subtree seen from the worktree.
#[test]
fn a_project_deny_holds_inside_a_worktree() {
    let role = ConfigFixture::new()
        .with_tool("write")
        .with_path(".", false, true)
        .with_path("secrets", false, false)
        .build();
    let denied = path_call("write", "secrets/k");
    let allowed = path_call("write", "src/k");
    assert_eq!(classify(&worktree_session(), &role, &denied), Verdict::Gate);
    assert_eq!(
        classify(&worktree_session(), &role, &allowed),
        Verdict::Clear
    );
}

/// Every session reaches every worktree: a plain session's write into another session's
/// worktree is judged as the project path.
#[test]
fn a_plain_session_reaches_any_worktree() {
    let role = ConfigFixture::new()
        .with_tool("write")
        .with_path(".", false, true)
        .build();
    let call = path_call("write", "/p/.efference/worktrees/other/src/x.rs");
    assert_eq!(
        classify(&Directories::at(Path::new("/p")), &role, &call),
        Verdict::Clear
    );
}

/// A grant key resolves against the project, not the working directory.
#[test]
fn a_grant_key_resolves_against_the_project() {
    let role = ConfigFixture::new()
        .with_tool("list")
        .with_path("../sib", true, false)
        .build();
    let call = path_call("list", "/sib/a");
    assert_eq!(classify(&worktree_session(), &role, &call), Verdict::Clear);
}
