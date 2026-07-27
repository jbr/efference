//! Folding config edits into the effective policy.

use super::*;

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
/// until an orchestrator commits it (docs/permissions.md).
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
