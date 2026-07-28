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
            thinking: vec![],
            text: vec![efference::log::ContentBlock::Text("hi there".into())],
            calls: vec![],
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
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("toolu_a".into()),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "rm  -rf\n ./build" }),
                }],
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
