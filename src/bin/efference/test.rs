use super::*;
use efference::boot::CONFIG_SCAFFOLD;
use efference::persist::Log;

/// Target resolution (docs/permissions.md): an explicit `--socket` wins over
/// everything and needs no listener; a `--name` with neither its own live socket nor
/// a host listing it is an error, not a blank UI. (Live discovery is exercised live.)
#[tokio::test]
async fn socket_override_beats_name() {
    let got = resolve_client_target(Some("foo"), Some(PathBuf::from("/tmp/x.sock")))
        .await
        .unwrap();
    assert_eq!(got.socket, PathBuf::from("/tmp/x.sock"));
    assert_eq!(got.prefix, None);
    assert_eq!(got.session_dir(), Some(PathBuf::from("/tmp")));
}

#[tokio::test]
async fn a_name_nothing_serves_is_an_error() {
    let err = resolve_client_target(Some("definitely-not-live"), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no live session"), "{err}");
    assert!(err.contains("definitely-not-live"), "{err}");
}

/// A hosted target's attachment store is the session dir beside the host socket.
#[test]
fn a_hosted_target_finds_its_session_dir() {
    let target = ClientTarget {
        socket: PathBuf::from(".efference/host.sock"),
        prefix: Some("/sessions/b".into()),
    };
    assert_eq!(target.session_dir(), Some(PathBuf::from(".efference/b")));
}

/// The session flags are refused without `--new`, since a plain `tui` attaches and
/// would otherwise silently drop them. `--model` has no default: omitted, the
/// host's applies.
#[test]
fn tui_session_flags_need_new() {
    use clap::Parser;
    let parse = |argv: &[&str]| Cli::try_parse_from(argv);
    let Command::Tui { new, session, .. } = parse(&["efference", "tui"]).unwrap().command else {
        panic!("tui")
    };
    assert!(!new && !session.any_given());
    let Command::Tui { new, session, .. } = parse(&["efference", "tui", "--model", "x"])
        .unwrap()
        .command
    else {
        panic!("tui")
    };
    assert!(!new && session.any_given());
    let Command::Tui {
        new,
        session,
        host_socket,
        ..
    } = parse(&["efference", "tui", "--new", "--effort", "high"])
        .unwrap()
        .command
    else {
        panic!("tui")
    };
    assert!(new && host_socket.is_none());
    assert!(session.model.is_none());
    assert_eq!(session.effort.as_deref(), Some("high"));
    assert!(session.triage().is_none());
    assert!(
        parse(&["efference", "tui", "--host-socket", "/tmp/h.sock"]).is_err(),
        "--host-socket requires --new"
    );
    let Command::Tui { session, .. } = parse(&[
        "efference",
        "tui",
        "--new",
        "--triage-model",
        "t",
        "--triage-thinking",
    ])
    .unwrap()
    .command
    else {
        panic!("tui")
    };
    let triage = session.triage().unwrap();
    assert!(triage.model == "t" && triage.thinking && !triage.auto_choice);
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
#[cfg(feature = "dev")]
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

#[cfg(feature = "dev")]
/// A minimal session log fixture: one user turn, one assistant reply. Written
/// via [`Log::create`]/[`Log::append`] (like a real session) so `persist::load`
/// reads back exactly what `wire` would see.
pub(crate) fn write_fixture_log(path: &Path) {
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

#[cfg(feature = "dev")]
#[test]
fn paint_is_plain_when_not_a_tty() {
    assert_eq!(paint("x", RED, false), "x");
    assert_eq!(paint("x", RED, true), "\x1b[31mx\x1b[0m");
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

/// What `efference dev cache` reads out of a session's `usage.jsonl`
/// (docs/log-and-projection.md): a per-origin warm rate, the cold main-loop reads
/// that carry their own explanation separated from the ones that do not, and the
/// longest run of the latter — the number the threshold is set against. A malformed
/// line is skipped rather than failing the report: the record is diagnostic, and its
/// last line can be a half-flushed write from a killed daemon.
#[cfg(feature = "dev")]
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
#[cfg(feature = "dev")]
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
