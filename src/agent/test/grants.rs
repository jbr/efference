//! Tests for grants, the config file, and the messages a refusal carries.
use super::*;

/// A committed grant reaches the **file**, not just the log fold
/// (docs/permissions.md). The session's `config.toml` is the
/// perimeter's readable form — what an operator inspects, and copies grants up to the origin
/// template from — so a grant that folded into effective policy but never appeared there would
/// make that artifact an incomplete record. Formatting survives, since the operator reads this
/// file by hand.
#[test]
pub(super) fn a_committed_grant_is_written_to_the_config_file() {
    use crate::log::ConfigChange;
    let path =
        std::env::temp_dir().join(format!("efference-writeback-{}.toml", std::process::id()));
    std::fs::write(&path, "# my perimeter\nscope = \"stay in src/\"\n").unwrap();
    let (shared, _rx) = Session::with_role(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
        crate::config::LoadedConfig {
            role: crate::config::Config::default(),
            source: crate::config::ConfigSource::Path(path.clone()),
        },
    );
    let call = bash_call("npm install");

    let approval = commit_config_edit(
        &shared,
        0,
        &call,
        ConfigChange::Command {
            command: "npm".into(),
        },
        "ok".into(),
    )
    .unwrap();
    assert_eq!(approval, Approval::Approve);

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("npm"), "the grant is in the file: {text}");
    assert!(
        text.contains("# my perimeter"),
        "the operator's formatting survives: {text}"
    );
    // The file and effective policy agree — no divergence between the two live records.
    assert!(shared.effective_config().commands.contains_key("npm"));
    assert!(
        crate::config::ConfigFile::load(&path)
            .unwrap()
            .config()
            .commands
            .contains_key("npm")
    );

    std::fs::remove_file(&path).unwrap();
}

/// Legislating a grant that *covers* the gated call amends the policy and approves the
/// call (the amended rule clears it on re-classify).
#[test]
pub(super) fn legislate_grants_then_approves_a_covered_call() {
    use crate::log::ConfigChange;
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let call = bash_call("npm install");
    assert_eq!(
        policy::classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Gate
    );

    let approval = commit_config_edit(
        &shared,
        0,
        &call,
        ConfigChange::Command {
            command: "npm".into(),
        },
        "ok".into(),
    )
    .unwrap();
    assert_eq!(approval, Approval::Approve);
    // The policy is now amended for any future call too.
    assert_eq!(
        policy::classify(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &shared.effective_config(),
            &call
        ),
        Verdict::Clear
    );
}

/// The invariant: a grant that does *not* cover the gated call still amends the policy,
/// but the call does **not** ride along — it's re-checked and stays gated (a redirect).
#[test]
pub(super) fn legislate_does_not_let_an_uncovered_call_ride_along() {
    use crate::log::ConfigChange;
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let call = bash_call("npm install");

    let approval = commit_config_edit(
        &shared,
        0,
        &call,
        ConfigChange::Tool {
            name: "read".into(),
        }, // unrelated to the bash call
        "unrelated".into(),
    )
    .unwrap();
    assert!(
        matches!(approval, Approval::Redirect(_)),
        "uncovered call stays gated"
    );
    // The change still landed — legislation happens regardless of the current call.
    assert!(shared.effective_config().tools.contains("read"));
}

/// The autonomous redirect names the config file (the policy interface the model edits to
/// request access), carries the spotter's reason, and makes the held-for-review property
/// explicit — so the model knows asking is possible but enactment isn't automatic.
#[test]
pub(super) fn autonomous_gate_message_points_at_the_config_and_carries_the_reason() {
    let msg = autonomous_gate_message(
        Some(Path::new(".efference/x/config.toml")),
        "writes outside the repo",
    );
    assert!(
        msg.contains(".efference/x/config.toml"),
        "names the config file"
    );
    assert!(
        msg.contains("writes outside the repo"),
        "carries the spotter reason"
    );
    assert!(msg.contains("autonomous mode"), "says why it didn't run");
    assert!(msg.contains("review"), "states the edit is held for review");
}

/// With no config file (the built-in default), the redirect still tells the model to ask the
/// orchestrator — it just can't name a file to edit.
#[test]
pub(super) fn autonomous_gate_message_without_a_config_file_still_redirects() {
    let msg = autonomous_gate_message(None, "network egress");
    assert!(msg.contains("orchestrator"));
    assert!(msg.contains("network egress"));
    assert!(!msg.contains("config.toml"));
}

/// A gated **structured** call gets the diagnosis, not just the verdict
/// (docs/permissions.md). The failure it answers: a model reads a gated
/// `edit`/`persist_edit` as a malfunction and starts rewriting files through
/// `cat > file <<EOF`. So the copy has to say the call never ran, name the missing grant in
/// the config's own vocabulary, point at the config-edit remedy, and close the bash detour.
#[test]
pub(super) fn a_gated_structured_edit_names_the_missing_grant_not_a_tool_defect() {
    let msg = missing_grant_message(
        Some(Path::new(".efference/x/config.toml")),
        "writes outside the repo",
        &crate::policy::MissingGrants {
            tool: Some("edit".into()),
            paths: vec!["src/lib.rs".into()],
            needs_write: true,
        },
    );
    assert!(
        msg.contains("did not run"),
        "the tool never executed: {msg}"
    );
    assert!(
        msg.contains("not about the tool's behavior") || msg.contains("behavior"),
        "rules out the tool-defect reading: {msg}"
    );
    assert!(msg.contains("`tools` does not list `edit`"), "{msg}");
    assert!(msg.contains("`[paths]`") && msg.contains("write"), "{msg}");
    assert!(msg.contains("src/lib.rs"), "names the target: {msg}");
    assert!(
        msg.contains(".efference/x/config.toml"),
        "the remedy: {msg}"
    );
    assert!(
        msg.contains("writes outside the repo"),
        "the spotter reason: {msg}"
    );
    // The route-around is closed and said to be closed.
    assert!(msg.contains("cat > "), "names the bash detour: {msg}");
    assert!(msg.contains("gates the same way"), "{msg}");
}

/// Only the halves that are actually missing are named — a call gated on the path alone does
/// not accuse the `tools` grant it has, and a read does not talk about writes.
#[test]
pub(super) fn the_missing_grant_message_names_only_what_is_missing() {
    let msg = missing_grant_message(
        None,
        "outside scope",
        &crate::policy::MissingGrants {
            tool: None,
            paths: vec!["/etc/hosts".into()],
            needs_write: false,
        },
    );
    assert!(!msg.contains("`tools`"), "the name grant is present: {msg}");
    assert!(msg.contains("grants `read`"), "{msg}");
    assert!(!msg.contains("cat > "), "no write detour to close: {msg}");
    assert!(msg.contains("orchestrator"), "no file to name: {msg}");
}

/// The decomposition behind that copy: the two halves are reported independently, so a call
/// missing only the tool name does not also claim its granted paths are ungranted (the path
/// check requires the name grant, which is true of the *decision* and misleading as a
/// *diagnosis*). `bash` has no such decomposition — its answer is a `[commands]` stance.
#[test]
pub(super) fn missing_grants_reports_the_two_halves_independently() {
    let role = crate::config::parse_text("[paths]\n\".\" = true\n");
    let call = |name: &str, input| ToolCall::new(name, "toolu_x", input);
    // Path granted, name not: only the name is reported.
    let missing = crate::policy::missing_grants(
        &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
        &role,
        &call("edit", serde_json::json!({ "file_path": "src/lib.rs" })),
    )
    .expect("gated");
    assert_eq!(missing.tool.as_deref(), Some("edit"));
    assert!(missing.paths.is_empty(), "{:?}", missing.paths);
    assert!(missing.needs_write);
    // bash is not decomposed this way.
    assert!(
        crate::policy::missing_grants(
            &crate::policy::Directories::at(&std::env::current_dir().unwrap()),
            &role,
            &call("bash", serde_json::json!({ "command": "rm -rf /" }))
        )
        .is_none()
    );
}

/// The gate-time half of the two-outcome rule (docs/permissions.md):
/// an approved config write that doesn't parse comes back to the model as an **error naming
/// the parse failure**, with the file reverted and policy unchanged — not as the success the
/// tool reported. This is the specific defect from session 1784334112726, where an approved
/// write reported success, changed nothing, and the model spent ~20 turns against a wall it
/// believed it had cleared.
#[test]
pub(super) fn a_config_write_that_does_not_parse_is_reverted_and_reported_as_an_error() {
    let path = std::env::temp_dir().join(format!(
        "efference-agent-config-{}.toml",
        std::process::id()
    ));
    let good = "# schema in this header\nscope = \"stay in src/\"\n";
    std::fs::write(&path, good).unwrap();
    let (shared, _rx) = crate::daemon::Session::with_role(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
        crate::config::LoadedConfig {
            role: crate::config::Config::default(),
            source: crate::config::ConfigSource::Path(path.clone()),
        },
    );

    // The model writes the footgun and the tool reports success, as it truthfully did write.
    std::fs::write(&path, "[scope]\ndefault = \"read\"\n").unwrap();
    let reported = config_write_outcome(&shared, RawResult::text("wrote 2 lines"));

    assert!(reported.is_error, "an inert edit is not a success");
    assert!(
        reported.content.contains("did not parse"),
        "names what went wrong: {}",
        reported.content
    );
    assert!(
        reported.content.contains("scope"),
        "carries the parse error itself: {}",
        reported.content
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        good,
        "the file is back to the last content that parsed"
    );

    // A valid edit passes straight through — the tool's own result stands.
    std::fs::write(&path, "scope = \"narrower\"\n").unwrap();
    let reported = config_write_outcome(&shared, RawResult::text("wrote 1 line"));
    assert!(!reported.is_error);
    assert_eq!(reported.content, "wrote 1 line");
    assert_eq!(
        shared.effective_config().scope.as_deref(),
        Some("narrower"),
        "an adopted edit is in force for the very next call"
    );

    std::fs::remove_file(&path).unwrap();
}
