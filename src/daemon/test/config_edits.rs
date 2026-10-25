//! Tests for `config_edits.rs`.

use super::*;

/// The effective-policy cache updates incrementally: a `ConfigEdit` appended mid-session
/// promotes a call the base gated into the fast-path, with no log re-scan at the gate
/// (docs/permissions.md).
#[test]
fn appending_a_config_edit_updates_the_effective_config() {
    use crate::policy::{Verdict, classify};
    let call = bash_call("npm install");

    // Empty base role → npm gates.
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Gate
    );

    // ConfigEdit `allow "npm"` → the same call now clears, via the incrementally-folded role.
    shared.append(0, allow_grant("npm")).unwrap();
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Clear
    );
}

/// A session-visible config edit (`scope`, `paths`) also raises a pending notification so the
/// session learns its perimeter moved; a classifier-only edit (`commands`/`tools`) does not
/// (docs/permissions.md).
#[test]
fn session_visible_config_edit_enqueues_a_notification() {
    let edit = committed_edit;

    // A scope edit notifies.
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert!(!shared.has_pending_notifications());
    shared
        .append(
            0,
            edit(crate::log::ConfigChange::Scope {
                text: "stay in src/".into(),
            }),
        )
        .unwrap();
    assert!(
        shared.has_pending_notifications(),
        "scope edit is session-visible"
    );

    // A path edit notifies.
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .append(
            0,
            edit(crate::log::ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true,
            }),
        )
        .unwrap();
    assert!(
        shared.has_pending_notifications(),
        "path edit is session-visible"
    );

    // A command edit does not — its effect reaches the model only through the classifier.
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.append(0, allow_grant("npm")).unwrap();
    assert!(
        !shared.has_pending_notifications(),
        "command edit is classifier-only"
    );
}

/// The proposal lifecycle (docs/permissions.md): a `Proposed` edit is
/// **inert** in the effective config but lands in the pending-proposals queue, addressed by its
/// own `seq`; an orchestrator's `Committed` decision (referencing that `seq` via `resolves`)
/// both applies the change *and* drains the proposal from the queue.
#[test]
fn a_proposal_is_inert_until_committed_then_drains_the_queue() {
    use crate::policy::{Verdict, classify};
    let call = bash_call("npm install");
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    // Propose: inert in policy, but queued.
    let proposal_seq = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "npm".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Gate,
        "a proposal must not take effect",
    );
    let queued = shared.pending_proposals();
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].seq, proposal_seq,
        "the proposal is addressed by its own seq"
    );

    // Commit it (referencing the proposal): the change applies and the queue drains.
    shared
        .append(
            0,
            Payload::ConfigEdit(crate::log::ConfigEdit {
                state: EditState::Committed,
                resolves: Some(proposal_seq),
                change: crate::log::ConfigChange::Command {
                    command: "npm".into(),
                },
                note: String::new(),
            }),
        )
        .unwrap();
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Clear
    );
    assert!(
        shared.pending_proposals().is_empty(),
        "commit drains the proposal"
    );
}

/// The fs-watch initiator's core (docs/permissions.md): reloading an
/// edited config file emits a `Committed` edit per delta against effective policy, folding it
/// in live; a session-visible delta (`scope`) also notifies; re-running against the *same* file
/// is a no-op echo (the diff is empty). Tests `refresh_and_emit` directly — the synchronous core
/// the notify thread drives — so there's no filesystem-event timing in the test.
#[test]
fn reload_emits_committed_edits_for_file_deltas() {
    use crate::policy::{Verdict, classify};
    let path = std::env::temp_dir().join(format!("efference-watch-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "scope = \"stay in src/\"\n[commands]\ncargo = true\n",
    )
    .unwrap();

    // The file is opened at construction, but effective policy still starts at the base role —
    // the fold happens on refresh, exactly as it does for an edit arriving mid-session.
    let (shared, _rx) = Session::with_role(
        std::env::current_dir().unwrap(),
        Log::in_memory(),
        file_role(&path),
    );
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &bash_call("cargo build")
        ),
        Verdict::Gate
    );

    // First reload folds the file's grants into effective policy and notifies on the scope.
    refresh_and_emit(&shared);
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &bash_call("cargo build")
        ),
        Verdict::Clear
    );
    assert_eq!(
        shared.effective_config().scope.as_deref(),
        Some("stay in src/")
    );
    assert!(
        shared.has_pending_notifications(),
        "scope delta is session-visible"
    );

    // A second reload against the unchanged file diffs to nothing — the idempotent echo.
    let before = shared.log.lock().unwrap().entries().len();
    refresh_and_emit(&shared);
    assert_eq!(
        shared.log.lock().unwrap().entries().len(),
        before,
        "no-op echo appends nothing"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A config edit that no longer parses keeps the last-good policy, **reverts the file**, and
/// surfaces the parse error to the session — the two-outcome rule
/// (docs/permissions.md). The revert is what makes the file an
/// always-valid record: the operator reads it to see the perimeter the session ran under.
#[test]
fn reload_failure_keeps_policy_reverts_file_and_notifies() {
    use crate::policy::{Verdict, classify};
    let path =
        std::env::temp_dir().join(format!("efference-watch-bad-{}.toml", std::process::id()));

    // A good edit first: grants `cargo`, moves policy off the default gate. The comment is
    // here to prove the revert restores bytes, not a re-render of the parsed config.
    let good = "# my config\nscope = \"stay in src/\"\n[commands]\ncargo = true\n";
    std::fs::write(&path, good).unwrap();
    let (shared, _rx) = Session::with_role(
        std::env::current_dir().unwrap(),
        Log::in_memory(),
        file_role(&path),
    );
    refresh_and_emit(&shared);
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &bash_call("cargo build")
        ),
        Verdict::Clear
    );

    // Now a malformed save — the exact footgun from session 1784334112726: `[scope]` as a
    // table where the schema wants a string, which fails the whole file.
    std::fs::write(
        &path,
        "[scope]\ndefault = \"read\"\n[commands]\ncargo = true\n",
    )
    .unwrap();
    refresh_and_emit(&shared);

    // Effective policy is unchanged (last good still in force) — fail-closed…
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &bash_call("cargo build")
        ),
        Verdict::Clear,
        "malformed edit keeps last-good policy"
    );
    // …the file itself is back to the last content that parsed, byte for byte…
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        good,
        "the invalid edit is reverted, comments and all"
    );
    // …but NOT silent: a notice on the parse-error slot tells the session its edit didn't take.
    assert!(
        shared
            .pending_notifications()
            .iter()
            .any(|n| n.slot.as_deref() == Some("config-parse-error")),
        "a parse failure raises a session-visible notice, distinct from the scope-delta notice"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A `Cancelled` decision drains the proposal from the queue and never touches policy.
#[test]
fn cancelling_a_proposal_drains_the_queue_without_applying() {
    use crate::policy::{Verdict, classify};
    let call = bash_call("npm install");
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    let proposal_seq = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "npm".into(),
            }),
        )
        .unwrap();
    assert_eq!(shared.pending_proposals().len(), 1);

    shared
        .append(
            0,
            Payload::ConfigEdit(crate::log::ConfigEdit {
                state: EditState::Cancelled,
                resolves: Some(proposal_seq),
                change: crate::log::ConfigChange::Command {
                    command: "npm".into(),
                },
                note: String::new(),
            }),
        )
        .unwrap();
    assert!(
        shared.pending_proposals().is_empty(),
        "cancel drains the proposal"
    );
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Gate,
        "a cancelled proposal never takes effect",
    );
}

/// The client-facing resolution path (`PATCH /proposals/:seq` → `resolve_proposal_decision`):
/// commit folds the change and drains the queue; cancel drains without applying; an unknown
/// `seq` resolves nothing.
#[test]
fn resolve_proposal_decision_commits_and_cancels() {
    use crate::policy::{Verdict, classify};
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());

    assert!(
        !shared
            .resolve_proposal_decision(999, true, String::new())
            .unwrap(),
        "an unknown seq resolves nothing",
    );

    // Commit: a proposed npm grant takes effect and drains.
    let npm = bash_call("npm install");
    let seq = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "npm".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &npm
        ),
        Verdict::Gate
    );
    assert!(
        shared
            .resolve_proposal_decision(seq, true, "looks safe".into())
            .unwrap()
    );
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &npm
        ),
        Verdict::Clear,
        "commit folds the proposed change",
    );
    assert!(
        shared.pending_proposals().is_empty(),
        "commit drains the queue"
    );

    // Cancel: a proposed cargo grant is dropped without taking effect.
    let cargo = bash_call("cargo build");
    let seq2 = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "cargo".into(),
            }),
        )
        .unwrap();
    assert!(
        shared
            .resolve_proposal_decision(seq2, false, "not now".into())
            .unwrap()
    );
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &cargo
        ),
        Verdict::Gate,
        "cancel does not apply the change",
    );
    assert!(
        shared.pending_proposals().is_empty(),
        "cancel drains the queue"
    );
}

/// On resume, grants already in the log fold into the *initial* effective role at
/// construction — the policy survives a restart even before any new grant lands.
#[test]
fn resumed_log_edits_fold_into_the_initial_effective_config() {
    use crate::policy::{Verdict, classify};
    let mut log = Log::in_memory();
    log.append(0, allow_grant("npm")).unwrap();

    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), log);
    assert_eq!(
        classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &bash_call("npm install")
        ),
        Verdict::Clear
    );
}

/// The one outcome the adopted-or-rejected invariant can't reach: the file fails to
/// parse **and** the restore fails too, leaving invalid content on disk
/// (docs/permissions.md). That parks the loop on an explicit "human must repair"
/// gate rather than running on under a policy the file no longer shows — and the
/// gate clears the way it was always going to: the human fixes the file, the
/// watcher's next refresh adopts it, and the park releases.
#[tokio::test]
async fn an_unrepairable_config_parks_the_loop_until_the_file_parses() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("efference-repair-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    let good = "# my config\nscope = \"stay in src/\"\n";
    std::fs::write(&path, good).unwrap();

    let (shared, _rx) = Session::with_role(
        std::env::current_dir().unwrap(),
        Log::in_memory(),
        file_role(&path),
    );
    refresh_and_emit(&shared);
    assert!(
        shared.config_repair().is_none(),
        "a parsing file parks nothing"
    );

    // Invalid content the harness cannot write back over: `[scope]` as a table, on a
    // file the process may read but not replace.
    std::fs::write(&path, "[scope]\ndefault = \"read\"\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::write(&path, "probe").is_ok() {
        // Running somewhere the mode doesn't bind (root); the failure this test needs
        // cannot be produced, so there is nothing to assert.
        std::fs::remove_dir_all(&dir).ok();
        return;
    }
    refresh_and_emit(&shared);

    let parked = shared.config_repair().expect("the loop is parked");
    assert!(parked.message.contains("human"), "{}", parked.message);
    assert!(
        parked.parse_error.contains("scope"),
        "{}",
        parked.parse_error
    );
    assert!(
        !parked.restore_error.is_empty(),
        "names why the restore failed"
    );
    // Terminal, not a notice: the wait does not return while the file is invalid.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shared.await_config_repair())
            .await
            .is_err(),
        "the park holds while the file is unparseable"
    );

    // The human repairs the file; the watcher picks it up; the gate clears.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&path, good).unwrap();
    refresh_and_emit(&shared);
    assert!(
        shared.config_repair().is_none(),
        "a parsing file clears the park"
    );
    tokio::time::timeout(Duration::from_secs(1), shared.await_config_repair())
        .await
        .expect("the park releases once the file parses");

    std::fs::remove_dir_all(&dir).ok();
}
