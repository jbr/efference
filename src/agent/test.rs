use super::*;
use crate::log::CallId;

/// `tool_catalog` covers every name `tool_definitions` advertises — the gap
/// review found: a catalog built from `Toolbox::tool_catalog` alone answered
/// only the mounted providers, so `GET /v1/tools/check_inbox` 404'd for a
/// tool the session's own `tools[]` carried.
#[test]
fn tool_catalog_covers_every_advertised_name() {
    let toolbox = Toolbox::bash_only();
    let advertised: std::collections::HashSet<String> = tool_definitions(&toolbox)
        .iter()
        .filter_map(|d| d.get("name")?.as_str().map(str::to_string))
        .collect();
    let catalog = tool_catalog(&toolbox);
    let cataloged: std::collections::HashSet<String> = catalog.keys().cloned().collect();
    assert_eq!(
        advertised, cataloged,
        "every wire name in tools[] has a catalog entry, and vice versa"
    );
}

/// A `Toolbox`-mounted tool's served schema carries the `intent` `with_intent`
/// injected on the real wire; a loop-owned tool's does not — `tool_definitions`
/// pushes `consolidate`/the supervisor family/the peek tools onto the array
/// *after* `with_intent` already ran over the toolbox half, so the model never
/// saw `intent` declared on any of them.
#[test]
fn only_the_toolbox_half_of_the_catalog_carries_injected_intent() {
    let toolbox = Toolbox::bash_only();
    let catalog = tool_catalog(&toolbox);

    let bash = &catalog[bash::NAME];
    assert!(
        bash.input_schema["properties"]["intent"].is_object(),
        "a toolbox-mounted tool's served schema matches the wire, intent included"
    );

    for name in [
        consolidate::NAME,
        supervisor::CHECK_INBOX,
        crate::distill::retain_tool()["name"].as_str().unwrap(),
        crate::distill::triage_tool()["name"].as_str().unwrap(),
    ] {
        let entry = &catalog[name];
        assert!(
            entry.server.is_none(),
            "{name} is the harness's own, not a mounted server's"
        );
        assert!(
            entry
                .input_schema
                .get("properties")
                .is_none_or(|p| p.get("intent").is_none()),
            "{name} was never sent an injected intent on the wire: {entry:?}"
        );
    }
}

pub(super) fn call(name: &str) -> ToolCall {
    ToolCall {
        id: CallId("toolu_x".into()),
        name: name.into(),
        input: serde_json::json!({}),
    }
}

pub(super) fn bash_call(command: &str) -> ToolCall {
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
pub(super) fn an_interrupted_call_is_paired_but_says_nothing_and_the_operator_speaks_next() {
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
                upstream_failure: None,
            },
        )
        .unwrap();
    shared
        .append(
            1,
            withheld(
                &call,
                RawResult {
                    images: Vec::new(),
                    content: "SECRET_TOKEN=hunter2\n…120k of log…".into(),
                    is_error: false,
                    content_elided: None,
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
pub(super) fn a_committed_grant_is_written_to_the_config_file() {
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
pub(super) fn legislate_does_not_let_an_uncovered_call_ride_along() {
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
    use crate::log::CallId;
    let role = crate::config::parse_text("[paths]\n\".\" = true\n");
    let call = |name: &str, input| ToolCall {
        id: CallId("toolu_x".into()),
        name: name.into(),
        input,
    };
    // Path granted, name not: only the name is reported.
    let missing = crate::policy::missing_grants(
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
pub(super) fn cap_passes_through_results_under_the_limit() {
    let r = ok("a".repeat(MAX_TOOL_RESULT_BYTES));
    let capped = cap_tool_result(&call("search"), r);
    assert!(!capped.is_error);
    assert_eq!(capped.content.len(), MAX_TOOL_RESULT_BYTES);
}

#[test]
pub(super) fn cap_turns_oversized_results_into_a_hard_error_with_a_head() {
    let huge = "x".repeat(MAX_TOOL_RESULT_BYTES + 1);
    let capped = cap_tool_result(&call("search"), ok(huge));
    assert!(capped.is_error, "over-cap result must be a hard error");
    // The result carries the explanation + head, not the megabytes.
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
pub(super) fn cap_head_is_char_boundary_safe() {
    // 4-byte chars straddling the 1KB head boundary.
    let huge = "🦀".repeat(MAX_TOOL_RESULT_BYTES); // 4 bytes each → well over the cap
    let capped = cap_tool_result(&call("read"), ok(huge));
    assert!(capped.is_error);
    // Round-trips as valid UTF-8 (the test would not even construct otherwise, but
    // assert the head contains whole crabs).
    assert!(capped.content.contains('🦀'));
}

/// A completion that carried several calls produces a **fully-paired** log, and
/// `project` merges the results into one user message (docs/README.md).
///
/// One tool call per turn is *requested* of the provider via
/// `disable_parallel_tool_use`, but it is only a request: no OpenRouter-routed endpoint
/// lists the equivalent knob in its supported parameters, so a provider may return
/// several calls and the flag is ignored by design. The loop therefore has to handle
/// N ≥ 1 rather than assume N = 1 — every `tool_use` gets exactly one `tool_result`, or
/// projection orphans it and the API rejects the request.
#[test]
pub(super) fn a_multi_call_completion_projects_a_fully_paired_wire() {
    use crate::project::ApiBlock;
    let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
    let first = ToolCall {
        id: crate::log::CallId("search_2".into()),
        name: "search".into(),
        input: serde_json::json!({}),
    };
    let second = ToolCall {
        id: crate::log::CallId("bash_3".into()),
        name: crate::bash::NAME.into(),
        input: serde_json::json!({ "command": "ls" }),
    };
    shared
        .append(
            1,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![first.clone(), second.clone()],
                upstream_failure: None,
            },
        )
        .unwrap();
    for (call, content) in [(&first, "hits"), (&second, "listing")] {
        shared
            .append(
                1,
                Payload::ToolResult {
                    call_id: call.id.clone(),
                    raw: RawResult {
                        images: Vec::new(),
                        content: content.into(),
                        is_error: false,
                        content_elided: None,
                    },
                    wire: WireState::Raw,
                },
            )
            .unwrap();
    }

    let messages = shared.project_messages();
    let assistant = &messages[messages.len() - 2];
    let uses: Vec<&str> = assistant
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(uses, vec!["search_2", "bash_3"]);

    // Both results land in a *single* trailing user message, in call order — the shape
    // Anthropic expects for a multi-call turn, and the one the fork peek reproduces.
    let last = messages.last().expect("a trailing user message");
    assert_eq!(last.role, crate::project::Role::User);
    let answered: Vec<&str> = last
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        answered, uses,
        "every tool_use is answered exactly once, in order"
    );
}

/// A frontend that must not be reached. A call naming no tool cannot run, so nothing
/// on the permission path — human gate included — should be asked about it.
struct NeverGates;

impl Frontend for NeverGates {
    async fn read_user_turn(&mut self) -> Option<crate::frontend::SubmittedTurn> {
        unreachable!("the loop is not running")
    }
    async fn approve(&mut self, call: &ToolCall) -> Approval {
        panic!(
            "a call naming no tool reached the human gate: {}",
            call.name
        )
    }
    async fn interrupted(&mut self) -> crate::frontend::Interrupt {
        unreachable!("no tool runs")
    }
}

/// The spotter rung, which must not be consulted either: asked about a nonexistent tool it
/// returns `clear`, with a rationale invented for the call it imagined
/// (journal/2026-08-07-session-key-review.md).
struct NeverAsked;

impl Spotter for NeverAsked {
    async fn assess(
        &self,
        _role: &crate::config::Config,
        call: &ToolCall,
        _brief: Option<&crate::tools::ToolBrief<'_>>,
        _probe: Option<&str>,
    ) -> crate::spotter::Spotting {
        panic!("a call naming no tool reached the spotter: {}", call.name)
    }
}

/// The existence check sits ahead of the whole permission cascade: a call naming no
/// advertised tool is answered with the corrective result text without any rung being
/// asked a question it has no way to answer (docs/permissions.md, docs/tools.md).
#[tokio::test]
async fn a_call_naming_no_tool_is_answered_ahead_of_the_cascade() {
    let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
    let toolbox = Toolbox::bash_only();
    let call = call("ferritin");
    let decision = run_call(&mut NeverGates, &shared, &NeverAsked, &toolbox, 1, &call)
        .await
        .expect("the check answers rather than erroring");
    let Decision::Direct(payloads) = decision else {
        panic!("an unrunnable call is answered directly, never run or gated");
    };
    let [Payload::ToolResult { call_id, raw, wire }] = &payloads[..] else {
        panic!("exactly one paired tool_result, or projection orphans the tool_use");
    };
    assert_eq!(call_id, &call.id);
    assert!(raw.is_error, "the call was not cogent");
    // `Direct` payloads never enter the triage gate, so the correction reaches the wire
    // verbatim — no summary stands between it and the model (docs/redaction-fork.md).
    assert!(matches!(wire, WireState::Raw));
    for expected in ["unknown tool: ferritin", "Mounted tools: bash", "`bash`"] {
        assert!(
            raw.content.contains(expected),
            "{expected:?} in {:?}",
            raw.content
        );
    }
}

/// The callable set is read off the turn's own `tools[]`, minus the peek tools — which
/// stand in every array for cache-lineage reasons but are never asked for on an agent
/// turn (docs/redaction-fork.md). Reading the same array is what keeps the check from
/// rejecting a tool the model was just shown.
#[test]
pub(super) fn callable_names_are_the_advertised_ones_minus_the_peek_tools() {
    let toolbox = Toolbox::bash_only();
    let callable = super::callable_names(&toolbox);
    for advertised in [crate::bash::NAME, crate::consolidate::NAME] {
        assert!(
            callable.iter().any(|n| n == advertised),
            "{advertised} is advertised, so a call may name it: {callable:?}"
        );
    }
    for peek in tool_definitions(&toolbox)
        .iter()
        .filter_map(|d| d.get("name")?.as_str())
        .filter(|n| is_peek_tool(n))
    {
        assert!(
            !callable.iter().any(|n| n == peek),
            "{peek} is advertised but never asked for on an agent turn"
        );
    }
}
