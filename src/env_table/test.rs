use super::*;
use crate::log::{ContentBlock, Payload, RawResult, WireState};

fn entry(mask: Option<bool>, strip: Option<bool>) -> EnvEntry {
    EnvEntry {
        mask,
        strip,
        ..EnvEntry::default()
    }
}

fn user(pairs: &[(&str, EnvEntry)]) -> BTreeMap<String, EnvEntry> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// ---------------------------------------------------------------- precedence

#[test]
fn exact_name_beats_glob() {
    let table = Table::merge(&user(&[("ANTHROPIC_API_KEY", entry(Some(false), None))]));
    // The built-in `*_KEY` says mask; the exact entry overrides it.
    assert_eq!(
        table.resolve("ANTHROPIC_API_KEY").0,
        EnvRule {
            mask: false,
            strip: false
        }
    );
    // A sibling that only the glob matches keeps the built-in answer.
    assert!(table.resolve("OTHER_KEY").0.mask);
}

#[test]
fn among_globs_more_literal_characters_wins() {
    let table = Table::merge(&user(&[
        ("*_KEY", entry(Some(false), None)),
        ("ACME_*_KEY", entry(Some(true), Some(true))),
    ]));
    assert_eq!(
        table.resolve("ACME_SIGNING_KEY").0,
        EnvRule {
            mask: true,
            strip: true
        }
    );
    // `*_KEY` (4 literals) still governs a name the longer glob misses.
    assert_eq!(
        table.resolve("OTHER_KEY").0,
        EnvRule {
            mask: false,
            strip: false
        }
    );
}

#[test]
fn equal_literal_count_globs_are_an_unresolvable_tie() {
    let table = Table::merge(&user(&[
        ("ACME_*", entry(Some(true), None)),
        ("*_KEYS", entry(None, Some(true))),
    ]));
    let (rule, conflict) = table.resolve("ACME_KEYS");
    let conflict = conflict.expect("two 5-literal globs match, neither is more specific");
    assert_eq!(conflict.name, "ACME_KEYS");
    // Sorted, because the table is a `BTreeMap` — the diagnostic reads the same
    // however the file was written.
    assert_eq!(conflict.keys, vec!["*_KEYS", "ACME_*"]);
    // The rule a non-failing caller falls back to is the union: fail closed, never
    // silently drop the entry.
    assert_eq!(
        rule,
        EnvRule {
            mask: true,
            strip: true
        }
    );
}

#[test]
fn precedence_is_never_file_order() {
    // Same two entries, opposite declaration order: identical resolution.
    let a = Table::merge(&user(&[
        ("ACME_*_KEY", entry(Some(true), None)),
        ("*_KEY", entry(Some(false), None)),
    ]));
    let b = Table::merge(&user(&[
        ("*_KEY", entry(Some(false), None)),
        ("ACME_*_KEY", entry(Some(true), None)),
    ]));
    assert_eq!(a.resolve("ACME_X_KEY").0, b.resolve("ACME_X_KEY").0);
    assert!(a.resolve("ACME_X_KEY").0.mask);
}

#[test]
fn an_unresolvable_tie_is_a_startup_error() {
    let report = EnvPolicy::build_from(
        &user(&[
            ("ACME_*", entry(Some(true), None)),
            ("*_KEYS", entry(None, Some(true))),
        ]),
        &env(&[("ACME_KEYS", "long-enough-value")]),
    );
    assert_eq!(report.conflicts.len(), 1);
    assert!(report.conflicts[0].to_string().contains("ACME_KEYS"));
}

// ------------------------------------------------------------------- the 2x2

#[test]
fn strip_and_mask_are_two_independent_questions() {
    let report = EnvPolicy::build_from(
        &user(&[
            // masked, not stripped: the secret a tool still needs.
            ("SERVICE_TOKEN", entry(Some(true), Some(false))),
            // stripped, not masked: env hygiene, and masking it would shred output.
            ("COLORTERM", entry(Some(false), Some(true))),
            // both.
            ("BUILD_SECRET", entry(Some(true), Some(true))),
            // neither.
            ("EDITOR", entry(Some(false), Some(false))),
        ]),
        &env(&[
            ("SERVICE_TOKEN", "tok-service-value"),
            ("COLORTERM", "truecolor"),
            ("BUILD_SECRET", "bld-secret-value"),
            ("EDITOR", "hx"),
        ]),
    );
    let policy = report.policy;
    let stripped = policy.child_env();
    let names: Vec<&str> = stripped.names().iter().map(String::as_str).collect();
    assert!(names.contains(&"COLORTERM"));
    assert!(names.contains(&"BUILD_SECRET"));
    assert!(!names.contains(&"SERVICE_TOKEN"));
    assert!(!names.contains(&"EDITOR"));

    let masked = policy.mask("tok-service-value truecolor bld-secret-value hx");
    assert!(!masked.contains("tok-service-value"));
    assert!(!masked.contains("bld-secret-value"));
    // The hygiene cell is *not* masked — that is the point of the 2x2.
    assert!(masked.contains("truecolor"));
}

#[test]
fn user_strip_composes_with_an_inherited_mask() {
    // Naming only `strip` at a built-in's key keeps the built-in's `mask = true`.
    let table = Table::merge(&user(&[("*_KEY", entry(None, Some(true)))]));
    assert_eq!(
        table.resolve("SOME_KEY").0,
        EnvRule {
            mask: true,
            strip: true
        }
    );
}

#[test]
fn built_in_defaults_mask_the_five_suffixes() {
    let table = Table::merge(&BTreeMap::new());
    for name in [
        "OPENAI_API_KEY",
        "WEBHOOK_SECRET",
        "GITHUB_TOKEN",
        "DB_PASSWORD",
        "GCP_CREDENTIALS",
    ] {
        assert!(table.resolve(name).0.mask, "{name} should mask by default");
        assert!(!table.resolve(name).0.strip, "{name} should not strip");
    }
    assert!(!table.resolve("PATH").0.mask);
}

// ------------------------------------------------------------- marker & floor

#[test]
fn the_marker_is_deterministic_and_names_the_variable() {
    let report = EnvPolicy::build_from(
        &BTreeMap::new(),
        &env(&[("ANTHROPIC_API_KEY", "sk-ant-secret-value")]),
    );
    let policy = report.policy;
    let expected = "⟦ANTHROPIC_API_KEY: sk-a… elided by harness⟧";
    assert_eq!(policy.mask("sk-ant-secret-value"), expected);
    // Same value → same marker, so "is it set?" and "does it match?" still answer.
    assert_eq!(
        policy.mask("a sk-ant-secret-value b sk-ant-secret-value"),
        format!("a {expected} b {expected}")
    );
}

#[test]
fn the_marker_prefix_is_fixed_not_proportional() {
    let short = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", "abcdefgh")])).policy;
    let long = EnvPolicy::build_from(
        &BTreeMap::new(),
        &env(&[("A_KEY", "abcdefghijklmnopqrstuvwxyz0123456789")]),
    )
    .policy;
    assert_eq!(short.mask("abcdefgh"), "⟦A_KEY: abcd… elided by harness⟧");
    assert_eq!(
        long.mask("abcdefghijklmnopqrstuvwxyz0123456789"),
        "⟦A_KEY: abcd… elided by harness⟧"
    );
}

#[test]
fn a_value_under_the_floor_is_reported_and_not_masked() {
    let report = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("SESSION_KEY", "dev")]));
    assert_eq!(report.below_floor, vec!["SESSION_KEY".to_string()]);
    assert_eq!(
        report.policy.mask("dev builds are dev"),
        "dev builds are dev"
    );
}

#[test]
fn a_value_exactly_at_the_floor_is_masked() {
    let value = "a".repeat(MASKABILITY_FLOOR);
    let report = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("X_KEY", &value)]));
    assert!(report.below_floor.is_empty());
    assert!(!report.policy.mask(&value).contains(&value));
}

#[test]
fn longest_needle_wins_when_two_secrets_nest() {
    let report = EnvPolicy::build_from(
        &BTreeMap::new(),
        &env(&[("A_KEY", "abcdefghij"), ("B_KEY", "abcdefghijklmn")]),
    );
    let masked = report.policy.mask("abcdefghijklmn");
    assert_eq!(masked, "⟦B_KEY: abcd… elided by harness⟧");
}

#[test]
fn a_marker_is_not_rescanned() {
    // The marker embeds the value's first four characters; the single left-to-right
    // pass advances past what it emitted, so no needle can match inside one.
    let report = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", "elided by harness")]));
    let once = report.policy.mask("elided by harness");
    assert_eq!(once.matches("elided by harness").count(), 1);
}

// ---------------------------------------------------------------- transforms

#[test]
fn base64_needles_cover_all_three_phase_offsets() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let value = "super-secret-token";
    let policy = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", value)])).policy;
    for lead in ["", "x", "xy"] {
        let blob = STANDARD.encode(format!("{lead}{value}").as_bytes());
        let masked = policy.mask(&blob);
        assert_ne!(masked, blob, "phase {} was not needled", lead.len());
    }
}

#[test]
fn url_encoded_and_json_escaped_forms_are_needled() {
    let value = "p@ss/word+with\"quote";
    let policy = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_PASSWORD", value)])).policy;
    let url = "https://x/?token=p%40ss%2Fword%2Bwith%22quote";
    assert!(!policy.mask(url).contains("p%40ss"));
    let json = serde_json::to_string(value).unwrap();
    assert!(!policy.mask(&json).contains("quote"));
}

#[test]
fn a_multiline_value_matches_line_wise() {
    // The PEM case: read back through `cat -n`, so no line is contiguous with the
    // next and the whole blob never appears.
    let pem = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkq\nhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----";
    let policy = EnvPolicy::build_from(&BTreeMap::new(), &env(&[("TLS_SECRET", pem)])).policy;
    let numbered =
        "     1\t-----BEGIN PRIVATE KEY-----\n     2\tMIIEvQIBADANBgkq\n     3\thkiG9w0BAQEFAASC\n";
    let masked = policy.mask(numbered);
    assert!(!masked.contains("MIIEvQIBADANBgkq"));
    assert!(!masked.contains("hkiG9w0BAQEFAASC"));
    // The line numbers themselves survive — only the value's lines go.
    assert!(masked.contains("     2\t"));
}

// ------------------------------------------------------------- append-only set

#[test]
fn the_needle_set_is_append_only_across_a_config_edit() {
    let policy = EnvPolicy::build_from(
        &BTreeMap::new(),
        &env(&[("EFFERENCE_TEST_ROTATED_KEY", "rotated-secret-value")]),
    )
    .policy;
    assert!(!policy.mask("rotated-secret-value").contains("rotated"));
    // An edit that stops masking the variable does not un-needle the value: it is
    // still a secret, and still in whatever files the session may read.
    policy.absorb_config(&user(&[(
        "EFFERENCE_TEST_ROTATED_KEY",
        entry(Some(false), None),
    )]));
    assert!(!policy.mask("rotated-secret-value").contains("rotated"));
}

#[test]
fn a_config_edit_adds_needles_for_a_newly_masked_variable() {
    let policy = EnvPolicy::build_from(
        &BTreeMap::new(),
        &env(&[("EFFERENCE_TEST_LATE_VALUE", "late-secret-value")]),
    )
    .policy;
    assert!(policy.mask("late-secret-value").contains("late-secret"));
    policy.absorb_config(&user(&[(
        "EFFERENCE_TEST_LATE_VALUE",
        entry(Some(true), None),
    )]));
    assert!(!policy.mask("late-secret-value").contains("late-secret"));
}

// ------------------------------------------------------------------- values

/// A resolver that answers one command from a fixed table — no test ever invokes
/// a real vault binary.
fn stub_resolver(answers: &'static [(&'static str, &'static str)]) -> Arc<Resolver> {
    Arc::new(Resolver::new(Box::new(move |command: &str| {
        answers
            .iter()
            .find(|(c, _)| *c == command)
            .map(|(_, v)| (*v).to_string())
            .ok_or_else(|| format!("stub has no answer for `{command}`"))
    })))
}

fn reference(value: &str) -> EnvEntry {
    EnvEntry {
        bare: Some(value.to_string()),
        ..EnvEntry::default()
    }
}

fn resolved(name: &str, value: &str, secret: bool) -> ResolvedValue {
    ResolvedValue {
        name: name.to_string(),
        value: value.to_string(),
        secret,
    }
}

#[test]
fn a_resolved_reference_is_injected_and_masked_by_construction() {
    // No table entry says `mask`, and the name matches no built-in glob — the
    // masking comes from the value having been fetched by the harness.
    let report = EnvPolicy::build_from_with(
        &user(&[("VAULT_CREDENTIAL", reference("op://V/I/f"))]),
        &[],
        &[resolved("VAULT_CREDENTIAL", "resolved-secret-value", true)],
        Arc::new(Resolver::default()),
    );
    let policy = report.policy;
    assert!(!policy.mask("resolved-secret-value").contains("resolved-s"));
    let child = policy.child_env();
    assert_eq!(
        child.injected_names().collect::<Vec<_>>(),
        vec!["VAULT_CREDENTIAL"]
    );
    // Injecting it is the point, so it is not stripped.
    assert!(!child.names().iter().any(|n| n == "VAULT_CREDENTIAL"));
}

#[test]
fn a_literal_value_is_injected_but_answers_the_ordinary_table_rules() {
    let report = EnvPolicy::build_from_with(
        &user(&[("RUST_LOG", reference("debug,efference=trace"))]),
        &[],
        &[resolved("RUST_LOG", "debug,efference=trace", false)],
        Arc::new(Resolver::default()),
    );
    let policy = report.policy;
    // A convenience literal is not a secret store: masking it would shred every
    // occurrence of the value in unrelated output.
    assert_eq!(
        policy.mask("debug,efference=trace"),
        "debug,efference=trace"
    );
    assert_eq!(
        policy.child_env().injected_names().collect::<Vec<_>>(),
        vec!["RUST_LOG"]
    );
}

#[test]
fn mask_false_on_a_reference_is_overruled_and_reported() {
    let report = EnvPolicy::build_from_with(
        &user(&[(
            "VAULT_CREDENTIAL",
            EnvEntry {
                mask: Some(false),
                bare: Some("op://V/I/f".into()),
                ..EnvEntry::default()
            },
        )]),
        &[],
        &[resolved("VAULT_CREDENTIAL", "resolved-secret-value", true)],
        Arc::new(Resolver::default()),
    );
    assert_eq!(report.forced_mask, vec!["VAULT_CREDENTIAL".to_string()]);
    assert!(
        !report
            .policy
            .mask("resolved-secret-value")
            .contains("resolved-s")
    );
}

#[test]
fn a_config_edit_needles_a_newly_named_reference() {
    let policy = EnvPolicy::build_from_with(
        &BTreeMap::new(),
        &[],
        &[],
        stub_resolver(&[("op read 'op://V/I/f'", "edit-time-secret-value")]),
    )
    .policy;
    assert!(policy.mask("edit-time-secret-value").contains("edit-time"));
    let errors = policy.absorb_config(&user(&[("LATE_CREDENTIAL", reference("op://V/I/f"))]));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!policy.mask("edit-time-secret-value").contains("edit-time"));
}

#[test]
fn a_config_edit_reports_a_reference_it_could_not_resolve() {
    let policy = EnvPolicy::build_from_with(&BTreeMap::new(), &[], &[], stub_resolver(&[])).policy;
    let errors = policy.absorb_config(&user(&[("LATE_CREDENTIAL", reference("op://V/Gone/f"))]));
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("op://V/Gone/f"));
}

// ------------------------------------------------------------ payload coverage

#[test]
fn every_text_bearing_payload_path_is_masked() {
    let policy =
        EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", "leaked-secret-value")])).policy;
    let masker = policy.masker();
    let leak = || "before leaked-secret-value after".to_string();

    let mut user_turn = Payload::User(vec![ContentBlock::Text(leak())]);
    masker.mask_payload(&mut user_turn);
    let Payload::User(blocks) = &user_turn else {
        unreachable!()
    };
    let ContentBlock::Text(text) = &blocks[0] else {
        unreachable!()
    };
    assert!(!text.contains("leaked-secret-value"));

    let mut result = Payload::tool_result("toolu_1", RawResult::text(leak()), WireState::Raw);
    masker.mask_payload(&mut result);
    let Payload::ToolResult { raw, .. } = &result else {
        unreachable!()
    };
    assert!(!raw.content.contains("leaked-secret-value"));

    let mut assistant = Payload::Assistant {
        output: crate::log::AssistantOutput::from_legacy_messages(
            vec![ContentBlock::Thinking {
                text: leak(),
                signature: "sig".into(),
            }],
            vec![ContentBlock::Text(leak())],
            vec![crate::log::ToolCall::new(
                "bash",
                "toolu_2",
                serde_json::json!({ "command": leak() }),
            )],
        ),
        upstream_failure: Some(leak().into()),
    };
    masker.mask_payload(&mut assistant);
    let Payload::Assistant {
        output,
        upstream_failure,
    } = &assistant
    else {
        unreachable!()
    };
    let blocks = output.messages().unwrap();
    let crate::log::MessagesOutputBlock::Text(text) = &blocks[1] else {
        unreachable!()
    };
    let crate::log::MessagesOutputBlock::ToolUse(call) = &blocks[2] else {
        unreachable!()
    };
    assert!(!text.contains("leaked-secret-value"));
    assert!(
        !call.input["command"]
            .as_str()
            .unwrap()
            .contains("leaked-secret-value")
    );
    assert!(
        !upstream_failure
            .as_ref()
            .unwrap()
            .why()
            .contains("leaked-secret-value")
    );
    // The documented exception: a thinking block carries a provider signature over
    // its exact text, and a modified block is rejected on replay.
    let crate::log::MessagesOutputBlock::Thinking { text, .. } = &blocks[0] else {
        unreachable!()
    };
    assert!(text.contains("leaked-secret-value"));

    let mut ambient = Payload::Ambient(crate::log::Notification {
        source: "tool".into(),
        body: leak(),
        slot: None,
    });
    masker.mask_payload(&mut ambient);
    let Payload::Ambient(note) = &ambient else {
        unreachable!()
    };
    assert!(!note.body.contains("leaked-secret-value"));

    let mut preamble = Payload::Preamble {
        system: leak(),
        context: crate::context::Resolved::default(),
    };
    masker.mask_payload(&mut preamble);
    let Payload::Preamble { system, .. } = &preamble else {
        unreachable!()
    };
    assert!(!system.contains("leaked-secret-value"));
}

/// The empty-stream capture is upstream text like any other, and a request's own
/// bytes can come back inside it — an edge that echoes a header, a `message_start`
/// that quotes the request. So the diagnostics pass the same scan the prose does:
/// header values, the `message_start` body, and the frames (docs/transport.md).
#[test]
fn captured_stream_diagnostics_are_masked() {
    let policy =
        EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", "leaked-secret-value")])).policy;
    let masker = policy.masker();
    let leak = || "before leaked-secret-value after".to_string();

    let mut payload = Payload::Assistant {
        output: crate::log::AssistantOutput::Messages(Vec::new()),
        upstream_failure: Some(crate::log::UpstreamFailureRecord {
            why: "stream finalized with no content and no usage".into(),
            stream: Some(crate::log::StreamDiagnostics {
                status: 200,
                headers: BTreeMap::from([("x-generation-id".to_string(), leak())]),
                message_start: Some(leak()),
                frames: leak(),
                frames_truncated: false,
            }),
        }),
    };
    masker.mask_payload(&mut payload);
    let Payload::Assistant {
        upstream_failure: Some(failure),
        ..
    } = &payload
    else {
        unreachable!()
    };
    let stream = failure.stream.as_ref().unwrap();
    assert!(!stream.headers["x-generation-id"].contains("leaked-secret-value"));
    assert!(
        !stream
            .message_start
            .as_deref()
            .unwrap()
            .contains("leaked-secret-value")
    );
    assert!(!stream.frames.contains("leaked-secret-value"));
}

#[test]
fn the_log_append_choke_point_filters_every_entry() {
    let policy =
        EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", "choke-point-secret")])).policy;
    let mut log = crate::persist::Log::in_memory();
    log.set_masker(policy.masker());
    log.append(
        1,
        Payload::User(vec![ContentBlock::Text("here: choke-point-secret".into())]),
    )
    .unwrap();
    let serialized = serde_json::to_string(&log.entries()[0]).unwrap();
    assert!(!serialized.contains("choke-point-secret"));
    assert!(serialized.contains("A_KEY"));
}

// ----------------------------------------------------------------- glob syntax

#[test]
fn glob_matching_is_star_only() {
    assert!(glob_match("*_KEY", "OPENAI_API_KEY"));
    assert!(!glob_match("*_KEY", "KEYRING"));
    assert!(glob_match("EFFERENCE_*", "EFFERENCE_BIND"));
    assert!(glob_match("A*C", "ABC"));
    assert!(glob_match("A*C", "AC"));
    assert!(!glob_match("A*C", "ABD"));
    assert!(glob_match("*", "anything"));
    assert!(glob_match("EXACT", "EXACT"));
    assert!(!glob_match("EXACT", "EXACTLY"));
}

/// The Responses argument string through append, log, and replay (docs/env.md,
/// docs/transport.md). The string is the replay authority, so masking rewrites it
/// only when a value changed: a clean call keeps the provider's exact bytes, a masked
/// one is re-serialized without the secret and still parses and pairs, the wire
/// carries the logged string byte for byte, and a string that does not parse is
/// scanned as text so the secret never reaches disk either way.
#[test]
fn a_responses_function_call_is_masked_on_append_and_replayed_as_logged() {
    use crate::api::{PromptCachePolicy, encode_responses};
    use crate::log::{AssistantOutput, CallId, ResponsesOutputItem, WireState};
    use crate::persist::Log;

    let policy =
        EnvPolicy::build_from(&BTreeMap::new(), &env(&[("A_KEY", "leaked-secret-value")])).policy;
    let mut log = Log::in_memory();
    log.set_masker(policy.masker());

    let call = |call_id: &str, arguments: &str| Payload::Assistant {
        output: AssistantOutput::Responses(vec![ResponsesOutputItem::FunctionCall {
            id: None,
            call_id: CallId(call_id.into()),
            name: "bash".into(),
            arguments: arguments.into(),
            status: None,
        }]),
        upstream_failure: None,
    };
    let result =
        |call_id: &str| Payload::tool_result(call_id, RawResult::text("ok"), WireState::Raw);
    // Provider spacing and key order, kept as sent when nothing is masked.
    let clean = r#"{"command": "ls",  "cwd": "/tmp"}"#;
    log.append(0, call("call_clean", clean)).unwrap();
    log.append(0, result("call_clean")).unwrap();
    log.append(
        0,
        call(
            "call_hot",
            r#"{"command": "curl -H 'x: leaked-secret-value'"}"#,
        ),
    )
    .unwrap();
    log.append(0, result("call_hot")).unwrap();
    log.append(
        0,
        call("call_broken", r#"{"command": "leaked-secret-value"#),
    )
    .unwrap();

    let entries = log.entries();
    let arguments = |i: usize| -> String {
        let Payload::Assistant { output, .. } = &entries[i].payload else {
            unreachable!()
        };
        let AssistantOutput::Responses(items) = output else {
            unreachable!()
        };
        let ResponsesOutputItem::FunctionCall { arguments, .. } = &items[0] else {
            unreachable!()
        };
        arguments.clone()
    };
    assert_eq!(
        arguments(0),
        clean,
        "a clean call keeps the provider's bytes"
    );

    // On disk: the same bytes the sink writes, with no secret anywhere.
    for entry in entries {
        let line = serde_json::to_string(entry).unwrap();
        assert!(!line.contains("leaked-secret-value"), "{line}");
    }

    // The masked call still parses and pairs.
    let Payload::Assistant { output, .. } = &entries[2].payload else {
        unreachable!()
    };
    let calls = output.tool_calls().expect("masked arguments still parse");
    assert_eq!(calls[0].id, CallId("call_hot".into()));
    let command = calls[0].input["command"].as_str().unwrap();
    assert!(
        command.contains("curl -H 'x: ") && !command.contains("leaked"),
        "{command}"
    );

    // The malformed string is masked as text and stays record-only.
    let broken = arguments(4);
    assert!(!broken.contains("leaked-secret-value"), "{broken}");
    let Payload::Assistant { output, .. } = &entries[4].payload else {
        unreachable!()
    };
    assert!(output.tool_calls().is_err(), "still unparseable: {broken}");

    // Replay carries the logged strings exactly.
    let input = encode_responses(
        &crate::project::project_semantic(entries),
        "",
        PromptCachePolicy::None,
    );
    let items: Vec<serde_json::Value> = input
        .iter()
        .map(|i| serde_json::to_value(i).unwrap())
        .collect();
    let wire_arguments: Vec<&str> = items
        .iter()
        .filter(|i| i["type"] == "function_call")
        .map(|i| i["arguments"].as_str().unwrap())
        .collect();
    assert_eq!(
        wire_arguments,
        vec![arguments(0), arguments(2), arguments(4)]
    );
    let outputs: Vec<&str> = items
        .iter()
        .filter(|i| i["type"] == "function_call_output")
        .map(|i| i["call_id"].as_str().unwrap())
        .collect();
    assert_eq!(outputs, vec!["call_clean", "call_hot"]);
    assert!(
        !serde_json::to_string(&items)
            .unwrap()
            .contains("leaked-secret-value")
    );
}

/// A host inference credential is masked below the ordinary noise floor, and its
/// marker carries the name only — a four-character prefix of a short token could
/// be most of it.
#[test]
fn absorb_secret_masks_short_values_without_a_prefix() {
    let masker = Masker::default();
    masker.absorb(&[("SHORT".into(), "abc".into())]);
    assert_eq!(masker.mask("key abc here"), "key abc here");
    masker.absorb_secret(&[("work (WORK_KEY)".into(), "abc".into())]);
    assert_eq!(
        masker.mask("key abc here"),
        "key ⟦work (WORK_KEY): elided by harness⟧ here"
    );
}
