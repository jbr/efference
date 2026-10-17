//! The command line's control verbs parse as documented (docs/operating.md).

use crate::cli::{Cli, Command};
use clap::Parser;
use efference::control_tools::{Decision, cli::Tools};

fn parse(argv: &[&str]) -> Result<Tools, clap::Error> {
    let mut full = vec!["efference", "control"];
    full.extend_from_slice(argv);
    match Cli::try_parse_from(full)?.command {
        Some(Command::Control(tools)) => Ok(tools),
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
    assert_eq!(message.agent.as_deref(), Some("child"));
    assert_eq!(message.words, ["do", "the", "thing"]);
    let Tools::Message(empty) = parse(&["message", "--agent", "child"]).unwrap() else {
        panic!("message")
    };
    assert!(empty.words.is_empty() && empty.text.is_empty());
    let Tools::Message(reply) = parse(&["message", "--reply-to", "3.1", "yes"]).unwrap() else {
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
    assert!(status.agent.is_none());
    let Tools::Transcript(audit) = parse(&["transcript", "--audit", "--agent", "child"]).unwrap()
    else {
        panic!("transcript")
    };
    assert!(audit.audit);
    let Tools::Transcript(zoom) = parse(&["transcript", "--seq", "8.2"]).unwrap() else {
        panic!("transcript")
    };
    assert_eq!(zoom.seq.as_deref(), Some("8.2"));
    assert!(parse(&["transcript", "--seq", "8", "--tail", "3"]).is_err());
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
