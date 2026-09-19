//! Tests for the `dev` group (docs/operating.md) — the wire view, the spotter-log
//! survey, and `peek`'s gate reconstruction. They live with the group so the
//! feature-off build compiles no test that names a feature-off item.

use super::*;
use crate::spot_report::{describe_call, spots_in};
use crate::test::write_fixture_log;

/// `wire --messages` is the point of this flag: a lean, config-independent
/// view of `project(log)` that works even when the session's config won't
/// parse. Exercise both halves — the full envelope path *does* fail on a
/// malformed config, while `--messages` skips config loading entirely and
/// succeeds regardless (docs: the flag's purpose is exactly this).
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

    // Without `--messages`, the full envelope rebuild loads the config and
    // fails loudly on the malformed file.
    let full = wire(
        log_path.clone(),
        false,
        WireFormat::Json,
        "claude-haiku-4-5",
        Some(bad_config.clone()),
    )
    .await;
    assert!(
        full.is_err(),
        "expected the full envelope to reject the bad config"
    );

    // With `--messages`, config is never touched, so the same bad config
    // doesn't matter — it prints just the projected messages array.
    let lean = wire(
        log_path.clone(),
        true,
        WireFormat::Json,
        "claude-haiku-4-5",
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

// ---- `peek` gate reconstruction (docs/redaction-fork.md) ----

use crate::peek_replay::gates;
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

/// A gate is a completion's calls **plus their results**, and its prefix ends *before* the
/// first of those results: the live loop snapshots the log at the assistant turn, and
/// `peek_messages` appends the results itself as fresh `tool_result` blocks. A prefix that
/// already carried them would double every result in the replayed request.
#[test]
fn a_gate_carries_a_completions_results_and_a_prefix_that_stops_short_of_them() {
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
    let gates = gates(&log);
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0].seq, 2, "addressed by its first result's seq");
    assert_eq!(gates[0].calls.len(), 2, "the whole completion's batch");
    assert_eq!(
        gates[0].prefix_end, 1,
        "the prefix is the assistant turn alone — the results are appended by the peek"
    );
}

/// The recorded first attempt is the baseline each replay is scored against, so it has to
/// come off the gate's own `Triage` rather than the session's aggregate.
#[test]
fn a_gate_carries_its_recorded_first_attempt() {
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
            vec!["attempt 1 of 3: peek called edit instead of retain".into()],
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
    let gates = gates(&log);
    assert_eq!(gates.len(), 2, "one gate per completion");
    assert!(
        gates[0]
            .recorded_first_failure
            .as_deref()
            .is_some_and(|w| w.contains("edit"))
    );
    assert_eq!(
        gates[1].recorded_first_failure, None,
        "this one complied first try"
    );
}

/// A completion whose calls never ran — aborted, interrupted, withheld — opens no gate,
/// so it must not appear as one with an empty result set.
#[test]
fn a_completion_with_no_results_opens_no_gate() {
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
    assert!(gates(&log).is_empty());
}
