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

/// A name resolves against the host path it is given, not only the default one:
/// the socket in the error is the one `efference mcp --host-socket` named.
#[tokio::test]
async fn a_name_resolves_against_the_given_host() {
    let err = resolve_client_target_at(
        vec![PathBuf::from("/nonexistent/efference/host.sock")],
        Some("definitely-not-live"),
        None,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("/nonexistent/efference/host.sock is not running"),
        "{err}"
    );
}

/// A host's listing is scoped to this directory: sessions with another cwd are
/// not candidates, and a listing with no cwd (an older host) is kept.
#[test]
fn a_hosts_listing_is_scoped_to_this_directory() {
    use efference::client::SessionSummary;
    let dir = std::env::temp_dir().join("efference-hosted-here");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let listing = |id: &str, cwd: Option<&str>| SessionSummary {
        id: id.into(),
        title: None,
        cwd: cwd.map(str::to_string),
        working: None,
        created_by: None,
        status: "idle".into(),
        processes: 0,
    };
    let listings = [
        listing("here", Some(dir.to_str().unwrap())),
        listing("dotted", Some(dir.join("sub/..").to_str().unwrap())),
        listing("elsewhere", Some(dir.join("sub").to_str().unwrap())),
        listing("unplaced", None),
    ];
    let got = hosted_here(&listings, &dir);
    assert_eq!(got, ["here", "dotted", "unplaced"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The candidate order is this project's host, then the user's.
#[test]
fn host_candidates_are_project_then_user() {
    let candidates = host_candidates();
    assert_eq!(candidates[0], PathBuf::from(".efference/host.sock"));
    if let Some(home) = std::env::var_os("HOME") {
        assert_eq!(candidates[1], Path::new(&home).join(".efference/host.sock"));
    }
}

/// The session flags are refused without `--new`, since a plain `tui` attaches and
/// would otherwise silently drop them. `--model` has no configured default:
/// omitted, past picks apply, else creation refuses and the form names one.
#[test]
fn tui_session_flags_need_new() {
    use clap::Parser;
    let parse = |argv: &[&str]| Cli::try_parse_from(argv);
    let Command::Tui { new, session, .. } = parse(&["efference", "tui"]).unwrap().command.unwrap()
    else {
        panic!("tui")
    };
    assert!(!new && !session.any_given());
    let Command::Tui { new, session, .. } = parse(&["efference", "tui", "--model", "x"])
        .unwrap()
        .command
        .unwrap()
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
        .unwrap()
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
    .unwrap() else {
        panic!("tui")
    };
    assert_eq!(
        session.triage(),
        Some(TriageChoice::separate("t".into(), true, false, None))
    );
    let Command::Tui { session, .. } = parse(&["efference", "tui", "--new", "--triage-shared"])
        .unwrap()
        .command
        .unwrap()
    else {
        panic!("tui")
    };
    assert_eq!(session.triage(), Some(TriageChoice::Shared));
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
    let mut log = Log::create(path, None).unwrap();
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
    let line = |origin, at_ms, cache_read| {
        let request = usage(10, 20, cache_read).with_model("claude-haiku-4-5");
        UsageLine::new(origin, at_ms, false, &request)
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

/// The provider's own attribution, folded (docs/transport.md): a named divergence is
/// counted by reason across a record, and the reasons that mean *no comparison happened*
/// are counted apart from them — they say nothing about the prefix, which is the
/// distinction a reader of a cold streak needs. A divergence named on a read that was
/// still warm is counted too: the provider saw the prompt move, and only an earlier
/// breakpoint happened to absorb it.
#[cfg(feature = "dev")]
#[test]
fn cache_report_counts_the_provider_attribution_by_reason() {
    use efference::api::{CacheMiss, CacheMissReason};
    use efference::daemon::RequestOrigin::Turn;
    use efference::daemon::UsageLine;

    let line = |at_ms, cache_read, miss: Option<(CacheMissReason, u64)>| {
        let request = usage(10, 20, cache_read);
        let request = match miss {
            Some((reason, missed)) => request.with_cache_miss(CacheMiss {
                reason,
                cache_missed_input_tokens: Some(missed),
            }),
            None => request,
        };
        UsageLine::new(Turn, at_ms, false, &request)
    };
    let t = 1_800_000_000_000u128;
    let lines = vec![
        line(t, 0, None), // boot
        line(t + 1_000, 0, Some((CacheMissReason::ToolsChanged, 41_850))),
        line(t + 2_000, 0, Some((CacheMissReason::ToolsChanged, 41_850))),
        line(t + 3_000, 0, Some((CacheMissReason::Unavailable, 0))),
        line(
            t + 4_000,
            5_000,
            Some((CacheMissReason::SystemChanged, 900)),
        ),
    ];
    let report = cache_report::fold_sessions(&[("s".into(), lines)]);

    let a = &report.attributed;
    assert_eq!(
        (
            a.model_changed,
            a.system_changed,
            a.tools_changed,
            a.messages_changed
        ),
        (0, 1, 2, 0),
        "the divergences, by the name the provider gave them"
    );
    assert_eq!(
        (a.unavailable, a.previous_message_not_found),
        (1, 0),
        "a no-comparison reason is held apart from the divergences"
    );
    assert_eq!((a.divergences(), a.total()), (3, 4));
}

/// Two sessions are folded with two trackers, never one: a prefix belongs to a
/// session, so the end of one must not be joined to the start of the next (which
/// would swallow the second session's boot cold start into the first's streak).
#[cfg(feature = "dev")]
#[test]
fn cache_report_does_not_join_streaks_across_sessions() {
    use efference::daemon::RequestOrigin::Turn;
    use efference::daemon::UsageLine;
    let cold = |at_ms| UsageLine::new(Turn, at_ms, false, &usage(1, 1, 0));
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

/// A request's usage with no cache write: the input to a test's [`UsageLine`].
///
/// [`UsageLine`]: efference::daemon::UsageLine
#[cfg(feature = "dev")]
fn usage(input: u64, output: u64, cache_read: u64) -> efference::api::RequestUsage {
    efference::api::RequestUsage::new(efference::api::Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cache_read,
    })
}

fn bind_flags(bind: Option<&str>) -> serve::BindFlags {
    serve::BindFlags {
        bind: bind.map(str::to_string),
        public_url: None,
        tls_cert: None,
        tls_key: None,
        open: false,
    }
}

/// Each flag wins over its `[host]` key alone (docs/web-bind.md): a flag bind keeps
/// the table's name and certificate, and `--bind ""` switches the table's bind off.
#[test]
fn bind_flags_override_the_host_table_key_by_key() {
    let table = efference::inference::HostConfig {
        bind: Some("100.64.0.1:7878".into()),
        public_url: Some("https://box.example.ts.net:7878".into()),
        tls_cert: Some("/c.crt".into()),
        tls_key: Some("/c.key".into()),
    };
    let merged = bind_flags(Some("127.0.0.1:0")).over(table.clone());
    assert_eq!(merged.bind.as_deref(), Some("127.0.0.1:0"));
    assert_eq!(
        merged.public_url.as_deref(),
        Some("https://box.example.ts.net:7878")
    );
    assert_eq!(merged.tls_key, Some(PathBuf::from("/c.key")));

    let off = bind_flags(Some("")).over(table.clone());
    assert!(serve::resolve_bind(off).unwrap().is_none());

    let from_table = bind_flags(None).over(table);
    assert_eq!(from_table.bind.as_deref(), Some("100.64.0.1:7878"));
}

/// Checks that clap made structural before the table existed now run after the
/// merge, since a pair can come half from each source.
#[test]
fn a_half_tls_pair_and_an_open_without_a_bind_are_refused() {
    let mut half = bind_flags(Some("127.0.0.1:0"));
    half.tls_cert = Some("/c.crt".into());
    let Err(err) = serve::resolve_bind(half) else {
        panic!("a half pair resolved");
    };
    let err = err.to_string();
    assert!(err.contains("go together"), "{err}");

    let mut open = bind_flags(None);
    open.open = true;
    let Err(err) = serve::resolve_bind(open) else {
        panic!("--open without a bind resolved");
    };
    let err = err.to_string();
    assert!(err.contains("--open needs a bind"), "{err}");
}

/// A bare port is the loopback address with that port, so the short spelling
/// cannot widen exposure past the rule that cleartext binds are loopback-only.
#[test]
fn a_bare_port_binds_loopback() {
    assert_eq!(serve::loopback_addr("8080"), "127.0.0.1:8080");
    assert_eq!(serve::loopback_addr("0"), "127.0.0.1:0");
    // Anything already addressed is left for the resolver to read as written.
    assert_eq!(serve::loopback_addr("127.0.0.1:8000"), "127.0.0.1:8000");
    assert_eq!(serve::loopback_addr("[::1]:8000"), "[::1]:8000");
    assert_eq!(serve::loopback_addr("localhost:8000"), "localhost:8000");
    assert_eq!(serve::loopback_addr("0.0.0.0:0"), "0.0.0.0:0");
}

/// A cleartext bind on a routable interface is refused, and the message says why
/// in terms of what an attacker gets: a replayed cookie drives the agent's
/// tools. It also names both ways out.
#[test]
fn a_routable_cleartext_bind_is_refused_with_the_reason() {
    let Err(err) = serve::resolve_bind(bind_flags(Some("0.0.0.0:8080"))) else {
        panic!("a routable cleartext bind resolved");
    };
    let err = err.to_string();
    assert!(err.contains("not loopback"), "{err}");
    assert!(err.contains("replay a session cookie"), "{err}");
    assert!(err.contains("--tls-cert/--tls-key"), "{err}");
}

/// A public URL is an origin, and one a TLS-terminating host prints is https; a
/// plaintext loopback listener behind a proxy may name an https one.
#[test]
fn a_public_url_is_an_https_origin_when_the_host_terminates_tls() {
    assert_eq!(
        serve::checked_public_url("https://Box.example.ts.net/", true).unwrap(),
        "https://box.example.ts.net"
    );
    assert!(serve::checked_public_url("https://box.example.ts.net", false).is_ok());
    assert!(serve::checked_public_url("http://box.example.ts.net:7878", true).is_err());
    assert!(serve::checked_public_url("https://box.example.ts.net/app", false).is_err());
    assert!(serve::checked_public_url("box.example.ts.net", false).is_err());
}

/// `--worktree` is a session flag: it sets the flag, and counts as a given flag so
/// a plain `tui` refuses it rather than dropping it.
#[test]
fn worktree_is_a_session_flag() {
    use clap::Parser;
    let parse = |argv: &[&str]| Cli::try_parse_from(argv);
    let Command::Tui { new, session, .. } = parse(&["efference", "tui", "--new", "--worktree"])
        .unwrap()
        .command
        .unwrap()
    else {
        panic!("tui")
    };
    assert!(new && session.worktree && session.any_given());
    let Command::Tui { session, .. } = parse(&["efference", "tui", "--new"])
        .unwrap()
        .command
        .unwrap()
    else {
        panic!("tui")
    };
    assert!(!session.worktree);
}
