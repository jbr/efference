
use super::*;
use crate::log::CallId;

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: CallId("toolu_x".into()),
        name: name.into(),
        input: serde_json::json!({}),
    }
}

fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        id: CallId("toolu_b".into()),
        name: crate::bash::NAME.into(),
        input: serde_json::json!({ "command": command }),
    }
}

/// The wire shape an interrupt produces (docs/control-channel.md). Three
/// things have to hold at once, and they are the whole design:
///
/// 1. The `tool_use` is **paired** — an orphan is a request the API rejects.
/// 2. The pair carries **none of the output** and no harness gloss: the point
///    of interrupting is that the tool's output never enters the context, and
///    a stand-in explaining the situation would be the harness guessing at a
///    meaning that differs per tool.
/// 3. The operator's next message merges into the **same** user message, so
///    the model reads `[tool_result, "…"]` in one breath — the person who
///    interrupted says why, in their own words, right where the output would
///    have been. That merge is `project`'s existing seam-drain behavior; this
///    pins that it survives the turn boundary an interrupt introduces.
#[test]
fn an_interrupted_call_is_paired_but_says_nothing_and_the_operator_speaks_next() {
    use crate::project::ApiBlock;
    let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
    let call = bash_call("cat enormous.log");
    shared
        .append(
            1,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![call.clone()],
            },
        )
        .unwrap();
    shared
        .append(
            1,
            withheld(
                &call,
                RawResult {
                    content: "SECRET_TOKEN=hunter2\n…120k of log…".into(),
                    is_error: false,
                },
            ),
        )
        .unwrap();
    // The turn ended; the operator's message opens the next one.
    shared
        .append(
            2,
            Payload::User(vec![ContentBlock::Text("read the test instead".into())]),
        )
        .unwrap();

    let messages = shared.project_messages();
    let last = messages.last().expect("a projected message");
    assert_eq!(last.role, crate::project::Role::User);
    match &last.content[..] {
        [
            ApiBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            },
            ApiBlock::Text { text, .. },
        ] => {
            assert_eq!(tool_use_id, &call.id.0, "the tool_use is paired");
            assert!(
                content.contains("[elided]"),
                "the pair is a placeholder: {content}"
            );
            assert!(
                !content.contains("hunter2") && !content.contains("120k"),
                "the withheld output never reaches the wire: {content}"
            );
            assert!(!is_error, "the call didn't fail — it was interrupted");
            assert_eq!(text, "read the test instead");
        }
        other => panic!("expected [tool_result, text] in one user message, got {other:?}"),
    }

    // Withheld from the model, not from the operator: `raw` is intact in the
    // log, so the TUI can still show it and the verdict stays reversible.
    let entries = shared.snapshot();
    let logged = entries
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { raw, wire, .. } => Some((raw, wire)),
            _ => None,
        })
        .expect("the result is in the log");
    assert!(logged.0.content.contains("hunter2"), "raw survives the log");
    assert!(matches!(logged.1, WireState::Elided));
}

/// A committed grant reaches the **file**, not just the log fold
/// (docs/permissions.md). The session's `config.toml` is the
/// perimeter's readable form — what an operator inspects, and copies grants up to the origin
/// template from — so a grant that folded into effective policy but never appeared there would
/// make that artifact an incomplete record. Formatting survives, since the operator reads this
/// file by hand.
#[test]
fn a_committed_grant_is_written_to_the_config_file() {
    use crate::log::ConfigChange;
    let path =
        std::env::temp_dir().join(format!("efference-writeback-{}.toml", std::process::id()));
    std::fs::write(&path, "# my perimeter\nscope = \"stay in src/\"\n").unwrap();
    let (shared, _rx) = Shared::with_role(
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
    assert!(shared.effective_config().commands.contains("npm"));
    assert!(
        crate::config::ConfigFile::load(&path)
            .unwrap()
            .config()
            .commands
            .contains("npm")
    );

    std::fs::remove_file(&path).unwrap();
}

/// Legislating a grant that *covers* the gated call amends the policy and approves the
/// call (the amended rule clears it on re-classify).
#[test]
fn legislate_grants_then_approves_a_covered_call() {
    use crate::log::ConfigChange;
    let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
    let call = bash_call("npm install");
    assert_eq!(
        policy::classify(&shared.effective_config(), &call),
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
        policy::classify(&shared.effective_config(), &call),
        Verdict::Clear
    );
}

/// The invariant: a grant that does *not* cover the gated call still amends the policy,
/// but the call does **not** ride along — it's re-checked and stays gated (a redirect).
#[test]
fn legislate_does_not_let_an_uncovered_call_ride_along() {
    use crate::log::ConfigChange;
    let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
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
fn autonomous_gate_message_points_at_the_config_and_carries_the_reason() {
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
fn autonomous_gate_message_without_a_config_file_still_redirects() {
    let msg = autonomous_gate_message(None, "network egress");
    assert!(msg.contains("orchestrator"));
    assert!(msg.contains("network egress"));
    assert!(!msg.contains("config.toml"));
}

/// The gate-time half of the two-outcome rule (docs/permissions.md):
/// an approved config write that doesn't parse comes back to the model as an **error naming
/// the parse failure**, with the file reverted and policy unchanged — not as the success the
/// tool reported. This is the specific defect from session 1784334112726, where an approved
/// write reported success, changed nothing, and the model spent ~20 turns against a wall it
/// believed it had cleared.
#[test]
fn a_config_write_that_does_not_parse_is_reverted_and_reported_as_an_error() {
    let path = std::env::temp_dir().join(format!(
        "efference-agent-config-{}.toml",
        std::process::id()
    ));
    let good = "# schema in this header\nscope = \"stay in src/\"\n";
    std::fs::write(&path, good).unwrap();
    let (shared, _rx) = crate::daemon::Shared::with_role(
        crate::persist::Log::in_memory(),
        crate::config::LoadedConfig {
            role: crate::config::Config::default(),
            source: crate::config::ConfigSource::Path(path.clone()),
        },
    );

    // The model writes the footgun and the tool reports success, as it truthfully did write.
    std::fs::write(&path, "[scope]\ndefault = \"read\"\n").unwrap();
    let reported = config_write_outcome(&shared, ok("wrote 2 lines".into()));

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
    let reported = config_write_outcome(&shared, ok("wrote 1 line".into()));
    assert!(!reported.is_error);
    assert_eq!(reported.content, "wrote 1 line");
    assert_eq!(
        shared.effective_config().scope.as_deref(),
        Some("narrower"),
        "an adopted edit is in force for the very next call"
    );

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn cap_passes_through_results_under_the_limit() {
    let r = ok("a".repeat(MAX_TOOL_RESULT_BYTES));
    let capped = cap_tool_result(&call("search"), r);
    assert!(!capped.is_error);
    assert_eq!(capped.content.len(), MAX_TOOL_RESULT_BYTES);
}

#[test]
fn cap_turns_oversized_results_into_a_hard_error_with_a_head() {
    let huge = "x".repeat(MAX_TOOL_RESULT_BYTES + 1);
    let capped = cap_tool_result(&call("search"), ok(huge));
    assert!(capped.is_error, "over-cap result must be a hard error");
    // The result no longer carries the megabytes; it carries the explanation + head.
    assert!(
        capped.content.len() < 4096,
        "the error is small, not the raw payload"
    );
    assert!(
        capped.content.contains("search"),
        "names the offending tool"
    );
    assert!(
        capped.content.contains("over the"),
        "states it crossed the limit"
    );
}

/// The head is cut on a char boundary, never mid-codepoint (it would panic / produce
/// invalid UTF-8 otherwise).
#[test]
fn cap_head_is_char_boundary_safe() {
    // 4-byte chars straddling the 1KB head boundary.
    let huge = "🦀".repeat(MAX_TOOL_RESULT_BYTES); // 4 bytes each → well over the cap
    let capped = cap_tool_result(&call("read"), ok(huge));
    assert!(capped.is_error);
    // Round-trips as valid UTF-8 (the test would not even construct otherwise, but
    // assert the head contains whole crabs).
    assert!(capped.content.contains('🦀'));
}
