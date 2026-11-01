//! The command line's control verbs parse as documented (docs/operating.md).

use crate::cli::{Cli, Command};
use clap::Parser;
use efference::clients::control_tools::{Output, cli::Tools};
use efference::protocol::agents::Decision;

fn parse(argv: &[&str]) -> Result<Tools, clap::Error> {
    let mut full = vec!["efference", "control"];
    full.extend_from_slice(argv);
    match Cli::try_parse_from(full)?.command {
        Some(Command::Control { tools, .. }) => Ok(tools),
        _ => panic!("not a control verb: {argv:?}"),
    }
}

/// `message` takes its words as positional arguments, joined by spaces, and the
/// target as `--agent`; with no words it is the empty message, a continue.
#[test]
fn message_takes_words_and_an_agent() {
    let Tools::Message(message) =
        parse(&["message", "--agent", "child", "do", "the", "thing"]).unwrap()
    else {
        panic!("message")
    };
    assert_eq!(message.agent.id(), "child");
    assert_eq!(message.words, ["do", "the", "thing"]);
    let Tools::Message(empty) = parse(&["message", "--agent", "child"]).unwrap() else {
        panic!("message")
    };
    assert!(empty.words.is_empty() && empty.text.is_empty());
    let Tools::Message(reply) =
        parse(&["message", "--agent", "child", "--reply-to", "3.1", "yes"]).unwrap()
    else {
        panic!("message")
    };
    assert_eq!(reply.reply_to.as_deref(), Some("3.1"));
}

/// `decide` takes the held call's seq and the decision positionally.
#[test]
fn decide_takes_a_seq_and_a_decision() {
    let Tools::Decide(decide) = parse(&[
        "decide",
        "--agent",
        "child",
        "8.2",
        "deny",
        "--message",
        "use the cache",
    ])
    .unwrap() else {
        panic!("decide")
    };
    assert_eq!(decide.seq, "8.2");
    assert_eq!(decide.decision, Decision::Deny);
    assert_eq!(decide.message.as_deref(), Some("use the cache"));
    assert!(parse(&["decide", "8.2", "maybe"]).is_err());
}

/// `status` with or without an agent; `transcript` with the audit grain on request.
#[test]
fn status_and_transcript_parse() {
    let Tools::Status(status) = parse(&["status"]).unwrap() else {
        panic!("status")
    };
    assert!(status.agent.is_empty() && !status.follow);
    assert_eq!(status.output, Output::Text);
    let Tools::Transcript(audit) = parse(&["transcript", "--audit", "--agent", "child"]).unwrap()
    else {
        panic!("transcript")
    };
    assert!(audit.audit);
    let Tools::Transcript(zoom) =
        parse(&["transcript", "--agent", "child", "--seq", "8.2"]).unwrap()
    else {
        panic!("transcript")
    };
    assert_eq!(zoom.seq.as_deref(), Some("8.2"));
    assert!(
        parse(&[
            "transcript",
            "--agent",
            "child",
            "--seq",
            "8",
            "--tail",
            "3"
        ])
        .is_err()
    );
}

/// The verbs that act on an agent name it, as the same verbs do over MCP; `status` is
/// the one that reads without.
#[test]
fn the_acting_verbs_require_an_agent() {
    for argv in [
        &["message", "hello"][..],
        &["decide", "8", "allow"],
        &["interrupt"],
        &["transcript"],
        &["archive"],
    ] {
        assert!(parse(argv).is_err(), "{argv:?} names no agent");
    }
    assert!(parse(&["status"]).is_ok());
}

/// `control` has no `--name` or `--socket`: a verb names its agent with `--agent`, and
/// `--host-socket` addresses a host. Before the verb, each is bare `efference`'s and is
/// refused beside a subcommand, as `main` parses.
#[test]
fn control_takes_neither_name_nor_socket() {
    for flag in ["--name", "--socket"] {
        for argv in [
            &["efference", flag, "x", "control", "state"][..],
            &["efference", "control", flag, "x", "state"],
            &["efference", "control", "state", flag, "x"],
        ] {
            let parsed = Cli::try_parse_from(argv).and_then(Cli::checked);
            assert!(parsed.is_err(), "{argv:?}");
        }
    }
}

/// The operator's route verbs stay on the command line, and the retired verbs are gone.
#[test]
fn the_operator_verbs_parse_and_the_retired_ones_do_not() {
    for argv in [
        &["state"][..],
        &["settle", "--timeout-s", "5"],
        &["pace", "--hold"],
        &["title", "a title"],
        &["background", "--note", "keep going"],
        &["proposal", "3", "looks safe"],
        &["review-level", "supervisor"],
        &["supervisor", "b", "--agent", "a"],
        &["notify", "test-watch", "build", "green", "--slot", "ci"],
        &["entries", "1", "2"],
        &["image", "3"],
        &[
            "create",
            "--model",
            "anthropic/claude-haiku-5.5",
            "--message",
            "go",
        ],
        &["interrupt", "--agent", "child"],
        &["archive", "child"],
    ] {
        assert!(parse(argv).is_ok(), "{argv:?}: {:?}", parse(argv).err());
    }
    for retired in ["turn", "approve", "sessions", "subscribe", "unsubscribe"] {
        assert!(parse(&[retired]).is_err(), "{retired} is retired");
    }
}

/// `--host-socket` is the host every verb addresses, before or after the verb;
/// `status` takes `--follow` with a repeated `--agent`, and `--output json`.
#[test]
fn host_socket_follow_and_output_parse() {
    let host_socket = |argv: &[&str]| {
        let mut full = vec!["efference", "control"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full).unwrap().command {
            Some(Command::Control { host_socket, .. }) => host_socket,
            _ => panic!("not a control verb"),
        }
    };
    let want = Some(std::path::PathBuf::from("./tmp/x.sock"));
    assert_eq!(
        host_socket(&["--host-socket", "./tmp/x.sock", "state"]),
        want
    );
    assert_eq!(
        host_socket(&["status", "--agent", "foo", "--host-socket", "./tmp/x.sock"]),
        want
    );
    assert_eq!(host_socket(&["state"]), None);

    let Tools::Status(follow) =
        parse(&["status", "--follow", "--agent", "a", "--agent", "b"]).unwrap()
    else {
        panic!("status")
    };
    assert!(follow.follow);
    assert_eq!(follow.agent, ["a", "b"]);
    let Tools::Status(json) = parse(&["status", "--output", "json"]).unwrap() else {
        panic!("status")
    };
    assert_eq!(json.output, Output::Json);
    assert!(parse(&["status", "--output", "yaml"]).is_err());
    assert!(parse(&["state", "--output", "json"]).is_err());
}

/// What `efference control --help` prints, and each verb's `--help` and `-h`, pinned whole
/// in `help.txt` (docs/operating.md): the verbs are declared once for every surface, so a
/// change to that declaration that moves a byte of the command line's text fails here. A
/// deliberate change updates the pin with `EFFERENCE_WRITE_PINS=1 cargo test help_is_pinned`.
#[test]
fn control_help_is_pinned() {
    use clap::CommandFactory;
    let command = Cli::command();
    let control = command
        .find_subcommand("control")
        .expect("`control` is a subcommand");
    let mut invocations = vec![vec!["--help".to_string()]];
    for verb in control.get_subcommands() {
        for flag in ["--help", "-h"] {
            invocations.push(vec![verb.get_name().to_string(), flag.to_string()]);
        }
    }
    let mut generated = String::new();
    for argv in invocations {
        let full = ["efference", "control"]
            .into_iter()
            .chain(argv.iter().map(String::as_str));
        let shown = match Cli::try_parse_from(full) {
            Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => e.render().to_string(),
            other => panic!("{argv:?} printed no help: {:?}", other.err()),
        };
        generated.push_str(&format!(
            "$ efference control {}\n{shown}\n",
            argv.join(" ")
        ));
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/efference/control/help.txt");
    if std::env::var_os("EFFERENCE_WRITE_PINS").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    if let Some((line, (was, now))) = committed
        .lines()
        .zip(generated.lines())
        .enumerate()
        .find(|(_, (was, now))| was != now)
    {
        panic!(
            "{} moved at line {}:\npinned:    {was}\ngenerated: {now}",
            path.display(),
            line + 1
        );
    }
    assert!(
        committed == generated,
        "{} differs from the generated help past its common lines",
        path.display()
    );
}
