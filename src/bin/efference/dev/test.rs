//! Tests for the `dev` group (docs/operating.md) — the wire view, the spotter-log
//! survey, and `triage`'s triage reconstruction. They live with the group so the
//! feature-off build compiles no test that names a feature-off item.

use super::*;
use crate::dev_models::parse_listing;
use crate::spot_report::{describe_call, spots_in};
use crate::test::write_fixture_log;

/// The two listing shapes in the wild are the OpenAI/Anthropic spelling and the
/// Codex backend's, and a body that is neither is a shape question rather than
/// a listing — so `dev models` shows the body instead of an empty menu, which
/// only `None` here distinguishes from a listing that is genuinely empty
/// (`{"models":[]}` parses to zero entries, and the verb prints that body).
#[test]
fn a_listing_is_read_in_either_spelling_and_unknown_shapes_are_refused() {
    assert_eq!(
        parse_listing(r#"{"data":[{"id":"gpt-5.6","display_name":"GPT-5.6"}]}"#),
        Some(vec![("gpt-5.6".to_string(), "GPT-5.6".to_string())])
    );
    // The Codex backend keys each entry by slug, and an entry with no display
    // name is labelled by its own id rather than dropped.
    assert_eq!(
        parse_listing(
            r#"{"models":[{"slug":"gpt-6-sol","display_name":"GPT-6-Sol"},{"slug":"gpt-5.5"}]}"#
        ),
        Some(vec![
            ("gpt-6-sol".to_string(), "GPT-6-Sol".to_string()),
            ("gpt-5.5".to_string(), "gpt-5.5".to_string()),
        ])
    );
    assert_eq!(parse_listing(r#"{"models":[]}"#), Some(vec![]));
    assert_eq!(parse_listing(r#"{"error":{"message":"no"}}"#), None);
    assert_eq!(parse_listing("not json"), None);
}

/// `wire --messages` is the point of this flag: a lean, config-independent
/// view of `project(log)` that works even when the session's config won't
/// parse. The full envelope still needs the role config when a legacy log has
/// no recorded preamble, so exercise both halves against a malformed file.
#[tokio::test]
async fn wire_messages_skips_the_unparseable_config() {
    let log_path =
        std::env::temp_dir().join(format!("efference-wire-log-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log_path);
    write_fixture_log(&log_path);

    let bad_config = std::env::temp_dir().join(format!(
        "efference-wire-bad-config-{}.toml",
        std::process::id()
    ));
    std::fs::write(&bad_config, "this is not valid toml [[[").unwrap();

    // This fixture predates the boot-preamble snapshot, so the full envelope
    // rebuild loads the role config and fails with the specific load error.
    let full = wire(
        log_path.clone(),
        false,
        WireFormat::Json,
        None,
        Some(bad_config.clone()),
    )
    .await;
    let error = full.expect_err("expected the full envelope to reject the bad config");
    assert_eq!(error.to_string(), "could not load role config");

    // The lean path never loads the config, so the same malformed file does not
    // matter; it returns the projected messages array.
    let lean = wire(
        log_path.clone(),
        true,
        WireFormat::Json,
        None,
        Some(bad_config.clone()),
    )
    .await;
    assert!(
        lean.is_ok(),
        "expected --messages to succeed despite the bad config: {lean:?}"
    );

    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&bad_config);
}

/// The full envelope's model field comes from the boot record, never from an
/// ambient provider selection.
#[test]
fn wire_model_prefers_the_recorded_native_model() {
    let identity = efference::log::LoggedInference::new(
        "router",
        efference::api::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    assert_eq!(wire_model(Some(&identity)), "google/gemini-3.7-flash");
    assert_eq!(wire_model(None), LEGACY_WIRE_MODEL);
}

/// Recorded identity owns the wire's provider-conditional shapes. Replacing it
/// with the default placement (or deriving the Gemini arm from a different model)
/// would silently change an image-bearing OpenRouter replay.
#[test]
fn wire_placements_follow_the_recorded_provider_and_model() {
    let identity = efference::log::LoggedInference::new(
        "router",
        efference::api::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    assert_eq!(
        wire_placements(Some(&identity)),
        (
            efference::project::ImagePlacement::AfterInterposedTurn,
            efference::project::BreakpointPlacement::None,
        )
    );
    assert_eq!(
        wire_placements(None),
        (
            efference::project::ImagePlacement::default(),
            efference::project::BreakpointPlacement::default(),
        )
    );
}

#[tokio::test]
async fn wire_rejects_a_recorded_provider_dialect_conflict() {
    let log_path = std::env::temp_dir().join(format!(
        "efference-wire-identity-conflict-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&log_path);
    let identity = efference::log::LoggedInference::new(
        "anthropic",
        efference::api::Provider::Anthropic,
        "claude",
    );
    let mut log = efference::persist::Log::create(&log_path, None).unwrap();
    log.append(
        0,
        efference::log::Payload::preamble("sys", Default::default(), Some(identity)),
    )
    .unwrap();
    log.append(
        1,
        efference::log::Payload::Assistant {
            output: efference::log::AssistantOutput::Responses(vec![]),
            upstream_failure: None,
        },
    )
    .unwrap();
    drop(log);

    for messages_only in [true, false] {
        let error = wire(
            log_path.clone(),
            messages_only,
            WireFormat::Json,
            None,
            None,
        )
        .await
        .expect_err("the recorded provider must be dialect-checked");
        let text = error.to_string();
        assert!(text.contains("recorded in the Responses dialect"), "{text}");
        assert!(text.contains("speaks Messages"), "{text}");
    }

    let _ = std::fs::remove_file(&log_path);
}

fn write_openrouter_image_log(path: &std::path::Path) {
    use efference::log::{
        AssistantOutput, ContentBlock, Payload, RawResult, ResultImage, ToolCall, WireState,
    };

    let identity = efference::log::LoggedInference::new(
        "router",
        efference::api::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    let mut log = efference::persist::Log::create(path, None).unwrap();
    log.append(
        0,
        Payload::preamble("sys", Default::default(), Some(identity)),
    )
    .unwrap();
    log.append(
        1,
        Payload::User(vec![ContentBlock::Text("look at the image".into())]),
    )
    .unwrap();
    log.append(
        2,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text("let me check".into())],
                vec![ToolCall::new(
                    "read_image",
                    "toolu_img",
                    serde_json::json!({ "path": "image.png" }),
                )],
            ),
            upstream_failure: None,
        },
    )
    .unwrap();
    log.append(
        3,
        Payload::tool_result(
            "toolu_img",
            RawResult::text("1456×819 image/webp, 1560 visual tokens").with_images(vec![
                ResultImage {
                    media_type: "image/webp".into(),
                    data: "UklGRg==".into(),
                    width: 1456,
                    height: 819,
                },
            ]),
            WireState::Raw,
        ),
    )
    .unwrap();
}

/// The lean command path must apply the recorded OpenRouter/Gemini placements,
/// not merely expose them through a helper: the output carries the interposed
/// assistant turn and carries no breakpoint.
#[tokio::test]
async fn wire_messages_renders_recorded_openrouter_placements_end_to_end() {
    let log_path = std::env::temp_dir().join(format!(
        "efference-wire-openrouter-placement-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&log_path);
    write_openrouter_image_log(&log_path);

    let rendered = wire(log_path.clone(), true, WireFormat::Json, None, None)
        .await
        .expect("the recorded OpenRouter fixture should render");
    let messages: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let messages = messages.as_array().expect("messages is an array");
    assert_eq!(
        messages.len(),
        5,
        "the image has an interposed turn: {rendered}"
    );
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[3]["role"], "assistant");
    assert!(
        messages[3]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("receiving the image from [3]")),
        "AfterInterposedTurn text is missing: {rendered}"
    );
    // A Gemini slug takes no marker; the default placement would have marked
    // the tail image, so its absence shows the recorded slug was used.
    assert!(
        !rendered.contains("cache_control"),
        "a Gemini replay carries no breakpoint: {rendered}"
    );
    assert_eq!(messages[4]["content"][0]["type"], "image");

    let _ = std::fs::remove_file(&log_path);
}

/// `--dialect completions` translates the same recorded session end to end: the
/// result's text answers its call as a `tool` message, and its image rides the
/// `user` message after it, since a `tool` message cannot carry one.
#[tokio::test]
async fn wire_messages_translates_to_completions_end_to_end() {
    let log_path = std::env::temp_dir().join(format!(
        "efference-wire-completions-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&log_path);
    write_openrouter_image_log(&log_path);

    let rendered = wire(
        log_path.clone(),
        true,
        WireFormat::Json,
        Some(ViewDialect::Completions),
        None,
    )
    .await
    .expect("the recorded OpenRouter fixture should translate");
    let messages: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let roles: Vec<_> = messages
        .as_array()
        .expect("messages is an array")
        .iter()
        .map(|message| message["role"].as_str().unwrap())
        .collect();
    let tool = roles
        .iter()
        .position(|role| *role == "tool")
        .expect("a tool message");
    assert_eq!(roles[tool - 1], "assistant", "{rendered}");
    assert_eq!(roles[tool + 1], "user", "{rendered}");
    assert_eq!(
        messages[tool + 1]["content"][0]["type"],
        "image_url",
        "{rendered}"
    );

    let _ = std::fs::remove_file(&log_path);
}

/// An identity-bearing full envelope is reconstructed from the log even when
/// the supplied config is malformed; only a legacy pre-preamble log needs the
/// current role config to rebuild its system prompt.
#[tokio::test]
async fn wire_full_uses_recorded_identity_without_loading_config() {
    let log_path = std::env::temp_dir().join(format!(
        "efference-wire-full-identity-{}.jsonl",
        std::process::id()
    ));
    let bad_config = std::env::temp_dir().join(format!(
        "efference-wire-full-bad-config-{}.toml",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&log_path);
    write_openrouter_image_log(&log_path);
    std::fs::write(&bad_config, "this is not valid toml [[[").unwrap();

    let rendered = wire(
        log_path.clone(),
        false,
        WireFormat::Json,
        None,
        Some(bad_config.clone()),
    )
    .await
    .expect("the recorded preamble should avoid config loading");
    let request: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(request["model"].as_str(), Some("google/gemini-3.7-flash"));
    assert_eq!(request["system"].as_str(), Some("sys"));
    let messages = request["messages"]
        .as_array()
        .expect("messages is an array");
    assert!(
        messages[3]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("receiving the image from [3]")),
        "the full envelope must retain the recorded OpenRouter placement: {request}"
    );
    assert!(
        !rendered.contains("cache_control"),
        "the full envelope must retain the recorded no-marker placement: {request}"
    );

    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&bad_config);
}

// ---- spotter-log survey ----

fn entry(seq: u64, payload: efference::log::Payload) -> efference::log::LogEntry {
    efference::log::LogEntry {
        seq,
        turn: 1,
        at: efference::log::Timestamp(0),
        submission_id: None,
        source: None,
        payload,
    }
}

/// A log with one bash call, a `Gate`(model) verdict on it, and a `Clear`(fallback)
/// verdict on a call id that isn't in the log (the fail-closed-on-a-missing-call shape).
fn spot_fixture() -> Vec<efference::log::LogEntry> {
    use efference::log::{CallId, Origin, Payload, Spot, Stance, ToolCall};
    vec![
        entry(
            1,
            Payload::Assistant {
                output: efference::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new(
                        "bash",
                        "toolu_a",
                        serde_json::json!({ "command": "rm  -rf\n ./build" }),
                    )],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            2,
            Payload::Spot(Spot {
                call_id: CallId("toolu_a".into()),
                stance: Stance::Gate,
                reason: "deletes outside the sanctioned scope".into(),
                origin: Origin::Model,
                model: None,
                probed_prefix: None,
                axes: None,
            }),
        ),
        entry(
            3,
            Payload::Spot(Spot {
                call_id: CallId("toolu_gone".into()),
                stance: Stance::Clear,
                reason: "spotter unavailable".into(),
                origin: Origin::Fallback,
                model: None,
                probed_prefix: None,
                axes: None,
            }),
        ),
    ]
}

#[test]
fn spots_in_correlates_calls_and_flattens_whitespace() {
    use efference::log::Stance;
    let rows = spots_in("sess", &spot_fixture(), &[], false);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].stance, Stance::Gate);
    // The bash command is resolved from the Assistant call and collapsed to one line.
    assert_eq!(rows[0].call, "bash: rm -rf ./build");
    // A Spot whose call id isn't in the log still surfaces, marked unresolved.
    assert!(rows[1].call.contains("unresolved"));
    assert!(rows[1].call.contains("toolu_gone"));
}

#[test]
fn spots_in_no_fallback_drops_fail_closed_verdicts() {
    use efference::log::Origin;
    let rows = spots_in("sess", &spot_fixture(), &[], true);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, Origin::Model);
}

#[test]
fn spots_in_stance_filter_keeps_only_matching() {
    use efference::log::Stance;
    let gates = spots_in("sess", &spot_fixture(), &[StanceArg::Gate], false);
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0].stance, Stance::Gate);
    // A stance nobody has → empty.
    assert!(spots_in("sess", &spot_fixture(), &[StanceArg::Watch], false).is_empty());
}

#[test]
fn describe_call_prefers_the_obvious_field() {
    assert_eq!(
        describe_call("bash", &serde_json::json!({ "command": "ls -la" })),
        "bash: ls -la"
    );
    assert_eq!(
        describe_call("read", &serde_json::json!({ "file_path": "src/log.rs" })),
        "read: src/log.rs"
    );
    // The structured read/list tools carry a `paths` array — joined, not dumped as JSON.
    assert_eq!(
        describe_call("read", &serde_json::json!({ "paths": ["a.rs", "b.rs"] })),
        "read: a.rs, b.rs"
    );
    // No obvious key → compact JSON of the whole input, so the call is never opaque.
    let other = describe_call("mystery", &serde_json::json!({ "z": 1, "a": 2 }));
    assert!(other.starts_with("mystery: "));
    assert!(other.contains("\"a\""));
}

// ---- `triage` triage reconstruction (docs/triage.md) ----

use crate::triage_replay::recorded_triages;
use efference::log::{LogEntry, Origin, Payload, RawResult, ToolCall, Triage, WireState};

fn call(id: &str) -> ToolCall {
    ToolCall::new("bash", id, serde_json::json!({"command": "ls"}))
}

fn result(seq: u64, id: &str) -> LogEntry {
    entry(
        seq,
        Payload::tool_result(id, RawResult::text("out"), WireState::Raw),
    )
}

fn triage(seq: u64, for_result: u64, retries: Vec<String>) -> LogEntry {
    entry(
        seq,
        Payload::Triage(Triage {
            for_result: Some(for_result),
            origin: Origin::Model,
            raw_keep: "all".into(),
            inbox: Vec::new(),
            surfaced: Vec::new(),
            retries,
            thinking: Vec::new(),
        }),
    )
}

/// A triage is a completion's calls **plus their results**, and its prefix ends *before* the
/// first of those results: the live loop snapshots the log at the assistant turn, and
/// `triage_messages` appends the results itself as fresh `tool_result` blocks. A prefix that
/// already carried them would double every result in the replayed request.
#[test]
fn a_triage_carries_a_completions_results_and_a_prefix_that_stops_short_of_them() {
    let log = vec![
        entry(
            1,
            Payload::Assistant {
                output: efference::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call("a"), call("b")],
                ),
                upstream_failure: None,
            },
        ),
        result(2, "a"),
        result(3, "b"),
        triage(4, 2, vec![]),
    ];
    let recorded = recorded_triages(&log);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].seq, 2, "addressed by its first result's seq");
    assert_eq!(recorded[0].calls.len(), 2, "the whole completion's batch");
    assert_eq!(
        recorded[0].prefix_end, 1,
        "the prefix is the assistant turn alone — the results are appended by the triage"
    );
}

/// The recorded first attempt is the baseline each replay is scored against, so it has to
/// come off the triage's own `Triage` rather than the session's aggregate.
#[test]
fn a_triage_carries_its_recorded_first_attempt() {
    let log = vec![
        entry(
            1,
            Payload::Assistant {
                output: efference::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call("a")],
                ),
                upstream_failure: None,
            },
        ),
        result(2, "a"),
        triage(
            3,
            2,
            vec!["attempt 1 of 3: triage called edit instead of retain".into()],
        ),
        entry(
            4,
            Payload::Assistant {
                output: efference::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call("b")],
                ),
                upstream_failure: None,
            },
        ),
        result(5, "b"),
        triage(6, 5, vec![]),
    ];
    let recorded = recorded_triages(&log);
    assert_eq!(recorded.len(), 2, "one triage per completion");
    assert!(
        recorded[0]
            .recorded_first_failure
            .as_deref()
            .is_some_and(|w| w.contains("edit"))
    );
    assert_eq!(
        recorded[1].recorded_first_failure, None,
        "this one complied first try"
    );
}

/// The inbox a replay passes is what the live triage was shown: notifications still
/// active in the prefix, plus those this triage first showed after its results (kept
/// even when its own verdict resolved them), and not ones an earlier triage resolved.
#[test]
fn a_triage_carries_the_inbox_it_was_shown() {
    use efference::log::{Disposition, Notification};
    let note = |seq, body: &str| {
        entry(
            seq,
            Payload::Ambient(Notification {
                source: "src".into(),
                body: body.into(),
                slot: None,
            }),
        )
    };
    let mut resolved = triage(3, 2, vec![]);
    if let Payload::Triage(t) = &mut resolved.payload {
        t.inbox = vec![(1, Disposition::Dismiss)];
    }
    let mut own = triage(8, 6, vec![]);
    if let Payload::Triage(t) = &mut own.payload {
        t.inbox = vec![(7, Disposition::Dismiss)];
    }
    let assistant = |seq, id| {
        entry(
            seq,
            Payload::Assistant {
                output: efference::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call(id)],
                ),
                upstream_failure: None,
            },
        )
    };
    let log = vec![
        note(1, "resolved earlier"),
        assistant(0, "a"),
        result(2, "a"),
        note(4, "left active"),
        resolved,
        assistant(5, "b"),
        result(6, "b"),
        note(7, "first shown here"),
        own,
    ];
    let recorded = recorded_triages(&log);
    let bodies =
        |i: usize| -> Vec<&str> { recorded[i].inbox.iter().map(|n| n.body.as_str()).collect() };
    assert_eq!(bodies(0), ["resolved earlier", "left active"]);
    assert_eq!(bodies(1), ["left active", "first shown here"]);
}

/// A completion whose calls never ran — aborted, interrupted, withheld — opens no triage,
/// so it must not appear as one with an empty result set.
#[test]
fn a_completion_with_no_results_opens_no_triage() {
    let log = vec![entry(
        1,
        Payload::Assistant {
            output: efference::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call("a")],
            ),
            upstream_failure: None,
        },
    )];
    assert!(recorded_triages(&log).is_empty());
}

#[test]
fn ladder_gaps_parse_comma_separated_seconds() {
    assert_eq!(
        crate::primer_ladder::parse_gaps("0,120,300,480").unwrap(),
        vec![0, 120, 300, 480]
    );
    assert!(crate::primer_ladder::parse_gaps("60, banana").is_err());
}
