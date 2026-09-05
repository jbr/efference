use super::*;

/// Socket resolution precedence (docs/permissions.md):
/// an explicit `--socket` wins over everything; a `--name` resolves to that session's colocated
/// socket. (The no-name discovery branch scans the filesystem and is exercised live.)
#[test]
fn socket_override_beats_name() {
    let got = resolve_client_socket(Some("foo"), Some(PathBuf::from("/tmp/x.sock"))).unwrap();
    assert_eq!(got, PathBuf::from("/tmp/x.sock"));
}

#[test]
fn name_resolves_to_its_colocated_socket() {
    let got = resolve_client_socket(Some("foo"), None).unwrap();
    assert_eq!(got, Path::new(".efference/foo/control.sock"));
}

#[test]
fn session_dir_is_under_the_base() {
    assert_eq!(session_dir("abc"), Path::new(".efference/abc"));
}

/// What `view` opens (docs/tui.md): a path to a log file is taken as given, a
/// directory resolves to the log inside it, and a bare name is read as a session id
/// under `.efference/`. A name that resolves to nothing is an error that lists what
/// there is — never a silent empty view. Uses a temp dir as the "session dir" so the
/// test doesn't depend on the repo's own `.efference/` contents.
#[test]
fn view_resolves_a_path_a_dir_or_a_name() {
    let dir = std::env::temp_dir().join(format!("efference-view-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join(LOG_NAME);
    write_fixture_log(&log_path);

    let (path, label) = resolve_session_log(log_path.to_str().unwrap()).unwrap();
    assert_eq!(path, log_path, "an explicit log path is taken as given");
    assert_eq!(label, log_path.to_str().unwrap());

    let (path, _) = resolve_session_log(dir.to_str().unwrap()).unwrap();
    assert_eq!(
        path, log_path,
        "a session dir resolves to the log inside it"
    );

    let err = resolve_session_log("definitely-not-a-session").unwrap_err();
    assert!(
        err.to_string().contains("no session"),
        "an unresolvable name errors: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The re-serve guard (docs/control-channel.md): a fresh name mints its dir, and a name whose
/// dir already exists is refused rather than opening a second seq space into the same log. The
/// error names the dir and the log inside it, so the operator can find the prior session.
#[test]
fn serve_mints_a_fresh_session_dir_and_refuses_an_existing_one() {
    use crate::serve::mint_session_dir;
    let base = std::env::temp_dir().join(format!("efference-mint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("sess");

    mint_session_dir(&dir).expect("a fresh name mints its dir");
    assert!(dir.is_dir(), "the parent is created recursively with it");

    let err = mint_session_dir(&dir).expect_err("a second serve of the same name is refused");
    let msg = err.to_string();
    assert!(
        msg.contains(&dir.display().to_string()),
        "names the dir: {msg}"
    );
    assert!(msg.contains(LOG_NAME), "points at the intact log: {msg}");

    // An empty dir is refused too — occupancy is the dir, not its contents.
    let bare = base.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert!(mint_session_dir(&bare).is_err());

    let _ = std::fs::remove_dir_all(&base);
}

/// A minimal session log fixture: one user turn, one assistant reply. Written
/// via [`Log::create`]/[`Log::append`] (like a real session) so `persist::load`
/// reads back exactly what `wire` would see.
fn write_fixture_log(path: &Path) {
    let mut log = Log::create(path).unwrap();
    log.append(
        0,
        efference::log::Payload::User(vec![efference::log::ContentBlock::Text("hello".into())]),
    )
    .unwrap();
    log.append(
        1,
        efference::log::Payload::Assistant {
            output: efference::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![efference::log::ContentBlock::Text("hi there".into())],
                vec![],
            ),
            upstream_failure: None,
        },
    )
    .unwrap();
}

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
                    vec![ToolCall {
                        id: CallId("toolu_a".into()),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "rm  -rf\n ./build" }),
                    }],
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

#[test]
fn paint_is_plain_when_not_a_tty() {
    assert_eq!(paint("x", RED, false), "x");
    assert_eq!(paint("x", RED, true), "\x1b[31mx\x1b[0m");
}

// ---- `peek` gate reconstruction (docs/redaction-fork.md) ----

use crate::peek_replay::gates;
use efference::log::{CallId, LogEntry, Origin, Payload, RawResult, ToolCall, Triage, WireState};

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: CallId(id.into()),
        name: "bash".into(),
        input: serde_json::json!({"command": "ls"}),
    }
}

fn result(seq: u64, id: &str) -> LogEntry {
    entry(
        seq,
        Payload::ToolResult {
            call_id: CallId(id.into()),
            raw: RawResult {
                images: Vec::new(),
                content: "out".into(),
                is_error: false,
                content_elided: None,
            },
            wire: WireState::Raw,
        },
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

/// The scaffold written for a session with no project config has to be a valid config, and
/// it has to list the guide overlay — that listing is the whole recovery path for a session
/// that knows nothing else (docs/context.md). Both halves, because a scaffold that stopped
/// parsing would fail every config-less boot and a scaffold that lost the entry would fail
/// silently.
#[test]
fn the_scaffold_parses_and_lists_the_guide() {
    let path = std::env::temp_dir().join(format!("efference-scaffold-{}.toml", std::process::id()));
    std::fs::write(&path, CONFIG_SCAFFOLD).unwrap();
    let loaded = efference::config::load(Some(&path));
    let _ = std::fs::remove_file(&path);
    let config = match loaded {
        Ok(loaded) => loaded.role,
        Err(e) => {
            e.eprint();
            panic!("the config scaffold must parse");
        }
    };
    assert!(
        config.context.iter().any(|entry| matches!(
            &entry.source,
            efference::config::ContextSource::Builtin(name) if name == efference::guide::BUILTIN
        )),
        "the scaffold must list the guide overlay"
    );
}

/// What `efference cache` reads out of a session's `usage.jsonl`
/// (docs/log-and-projection.md): a per-origin warm rate, the cold main-loop reads
/// that carry their own explanation separated from the ones that do not, and the
/// longest run of the latter — the number the threshold is set against. A malformed
/// line is skipped rather than failing the report: the record is diagnostic, and its
/// last line can be a half-flushed write from a killed daemon.
#[test]
fn cache_report_separates_explained_cold_reads_from_the_streak() {
    use efference::daemon::UsageLine;
    let line = |origin, at_ms, cache_read| UsageLine {
        at_ms,
        origin,
        id: None,
        model: Some("claude-haiku-4-5".into()),
        input: 10,
        output: 20,
        cache_read,
        cache_write: 0,
    };
    use efference::daemon::RequestOrigin::{Keepalive, Spot, Turn};
    let t = 1_800_000_000_000u128;
    let lines = vec![
        line(Turn, t, 0),               // boot — explained
        line(Turn, t + 1_000, 5_000),   // warm
        line(Spot, t + 1_500, 0),       // a side channel, invisible to the streak
        line(Turn, t + 2_000, 0),       // unexplained ×1
        line(Turn, t + 3_000, 0),       // unexplained ×2
        line(Keepalive, t + 4_000, 60), // touches the prefix
        line(Turn, t + 5_000, 0),       // unexplained ×3
    ];
    let report = cache_report::fold_sessions(&[("s".into(), lines)]);

    assert_eq!(report.sessions, 1);
    let turn = report
        .by_origin
        .iter()
        .find(|(o, _)| *o == Turn)
        .map(|(_, t)| *t)
        .expect("a turn row");
    assert_eq!((turn.requests, turn.warm), (5, 1));
    assert_eq!(
        report.cold_starts,
        [1, 0, 0],
        "the session's first request is a boot cold start, not evidence"
    );
    assert_eq!(report.unexplained, 3);
    assert_eq!(report.longest_streak, 3);
    assert_eq!(
        report.final_streak, 3,
        "the record ends mid-streak — what the sidebar would be showing"
    );
}

/// Two sessions are folded with two trackers, never one: a prefix belongs to a
/// session, so the end of one must not be joined to the start of the next (which
/// would swallow the second session's boot cold start into the first's streak).
#[test]
fn cache_report_does_not_join_streaks_across_sessions() {
    use efference::daemon::RequestOrigin::Turn;
    use efference::daemon::UsageLine;
    let cold = |at_ms| UsageLine {
        at_ms,
        origin: Turn,
        id: None,
        model: None,
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
    };
    let t = 1_800_000_000_000u128;
    let report = cache_report::fold_sessions(&[
        ("a".into(), vec![cold(t), cold(t + 1_000)]),
        ("b".into(), vec![cold(t + 2_000), cold(t + 3_000)]),
    ]);
    assert_eq!(report.sessions, 2);
    assert_eq!(report.cold_starts[0], 2, "each session boots cold once");
    assert_eq!(report.unexplained, 2);
    assert_eq!(report.longest_streak, 1);
}
