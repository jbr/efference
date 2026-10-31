//! The status as each reader reads it, pinned as exact text.

use super::*;
use crate::log::{Axes, CallId, Origin, Precedent, Spot, Stance};

mod gate;

const NOW: u64 = 1_754_300_000_000;
const SECOND: u64 = 1000;
const MINUTE: u64 = 60 * SECOND;

fn view(condition: Condition) -> StatusView {
    StatusView {
        id: "child".into(),
        condition,
        footer: Footer::default(),
    }
}

fn reviewer() -> Reader<'static> {
    Reader::notification(Relation::Reviewer, "parent")
}

fn driver() -> Reader<'static> {
    Reader::notification(Relation::Driver, "parent")
}

fn observer() -> Reader<'static> {
    Reader::notification(Relation::Observer, "peer")
}

/// A read rather than a notification: the footer is printed.
fn read(relation: Relation) -> Reader<'static> {
    Reader {
        footer: true,
        ..Reader::notification(relation, "parent")
    }
}

fn rendered(view: &StatusView, reader: &Reader) -> String {
    render(view, reader, NOW)
}

/// The spotter's verdict on `toolu_1`, from the decision rung when `axes` is given.
fn spot(origin: Origin, reason: &str, axes: Option<Axes>) -> Spot {
    Spot {
        call_id: CallId("toolu_1".into()),
        stance: Stance::Gate,
        reason: reason.into(),
        origin,
        model: None,
        probed_prefix: None,
        axes,
    }
}

/// Answers clean on every axis but the ones a case sets.
fn axes(sketch_level: usize, precedent: Option<Precedent>) -> Axes {
    Axes {
        in_scope: 0.95,
        path_outside: 0.02,
        path_denied: 0.01,
        exposes_secrets: 0.01,
        sketch: Some(
            (0..5)
                .map(|i| if i == sketch_level { 0.8 } else { 0.05 })
                .collect(),
        ),
        precedent,
        ..Axes::default()
    }
}

fn precedent(candidates: usize, same: usize, required: Option<usize>) -> Precedent {
    Precedent {
        candidates,
        same,
        wider: 0,
        required,
        compared: Vec::new(),
    }
}

/// A held `bash` call at `8.2`, held for what `spot` says and routed to `reviewer`.
fn gated(spot: &Spot, reviewer: Reviewer) -> StatusView {
    view(Condition::Gated(HeldCall {
        call_id: "toolu_1".into(),
        tool: "bash".into(),
        summary: Some("curl -sI https://example.com".into()),
        intent: Some("fetch only the response headers".into()),
        handle: Some("8.2".into()),
        held_because: held_because(spot),
        reviewer,
        escalations: Vec::new(),
    }))
}

fn supervisor(id: &str) -> Reviewer {
    Reviewer::Supervisor { id: id.into() }
}

const DECIDE: &str = "decide: agents__decide(agent: \"child\", seq: \"8.2\", decision: \"allow\" | \
    \"deny\", message?) — allow runs it, and a message rides with its result; deny with a message \
    refuses it and the session goes on under your message; a bare deny ends its turn";

/// The escalate line a reviewer reads after [`DECIDE`].
const ESCALATE: &str = "escalate: agents__decide(agent: \"child\", seq: \"8.2\", decision: \
    \"escalate\", message?) — leaves it held and passes it to whoever decides above you, with \
    your message as a note to them";

/// A question gives its place and handle; a driver is told a message answers it.
#[test]
fn asking_n_of_m() {
    let asking = view(Condition::Asking {
        question: "Which branch should I use?".into(),
        n: 2,
        of: 3,
        handle: Some("14".into()),
        returned: vec![],
    });
    let first = "child: asking a question (question 2 of 3, seq 14)\n\
        question: Which branch should I use?";
    assert_eq!(
        rendered(&asking, &driver()),
        format!(
            "{first}\nanswer: agents__message(agent: \"child\", text: \"<answer>\", reply_to: \
             \"14\") answers it"
        )
    );
    assert_eq!(rendered(&asking, &observer()), first);

    let lone = view(Condition::Asking {
        question: "Sure?".into(),
        n: 1,
        of: 1,
        handle: None,
        returned: vec![],
    });
    assert!(rendered(&lone, &observer()).starts_with("child: asking a question\n"));
}

/// A message the question returned undelivered is said to its sender, in-session and over
/// MCP, and to no one else.
#[test]
fn a_returned_message_is_said_to_its_sender() {
    let asking = view(Condition::Asking {
        question: "Sure?".into(),
        n: 1,
        of: 1,
        handle: Some("9".into()),
        returned: vec![
            Returned {
                source: Some("session[parent]".into()),
                text: "use main".into(),
            },
            Returned {
                source: Some("mcp[claude-code]".into()),
                text: "stop".into(),
            },
        ],
    });
    let line = |text: &str| {
        format!(
            "returned: your message {text:?} was returned undelivered; answer first, then \
             resend it if it still applies"
        )
    };
    let parent = rendered(&asking, &driver());
    assert!(parent.ends_with(&line("use main")), "{parent}");
    assert!(!parent.contains("\"stop\""), "{parent}");
    let mount = rendered(
        &asking,
        &Reader::read(Relation::Driver, &Moves::MCP, Some("claude-code")),
    );
    assert!(mount.contains(&line("stop")), "{mount}");
    assert!(!mount.contains("use main"), "{mount}");
    assert!(!rendered(&asking, &observer()).contains("returned:"));
}

fn idle(text: &str, bytes: usize, lines: usize) -> StatusView {
    view(Condition::Idle {
        closing: Some(Closing {
            seq: 12,
            text: text.into(),
            bytes,
            lines,
        }),
    })
}

const NEXT: &str = "next: agents__message(agent: \"child\", text: \"<message>\") sends its next \
    message; agents__archive(agent: \"child\") ends it";

/// A driver reads the whole closing message; an observer reads its first line and how
/// much was left.
#[test]
fn idle_with_a_short_message() {
    let short = idle("done: two files changed", 23, 1);
    assert_eq!(
        rendered(&short, &driver()),
        format!("child: idle, its turn is done (seq 12)\nsaid: done: two files changed\n{NEXT}")
    );
    assert_eq!(
        rendered(&short, &observer()),
        "child: idle, its turn is done (seq 12)\nsaid: done: two files changed"
    );

    let message = "Done.\nTwo files changed:\n- a.rs\n- b.rs";
    let several = idle(message, message.len(), 4);
    assert_eq!(
        rendered(&several, &driver()),
        format!("child: idle, its turn is done (seq 12)\nsaid: {message}\n{NEXT}")
    );
    assert_eq!(
        rendered(&several, &observer()),
        "child: idle, its turn is done (seq 12)\nsaid: Done.\ncut: 3 more lines; \
         agents__transcript(agent: \"child\", seq: \"12\") shows the whole message"
    );

    let silent = view(Condition::Idle { closing: None });
    assert_eq!(
        rendered(&silent, &driver()),
        format!("child: idle, its turn is done\n{NEXT}")
    );
}

/// Past the cap a driver is told how much was cut and which call reads the rest.
#[test]
fn idle_with_a_message_past_the_cap() {
    let text = "x".repeat(CLOSING_CAP);
    let long = idle(&text, CLOSING_CAP + 1024, 1);
    assert_eq!(
        rendered(&long, &driver()),
        format!(
            "child: idle, its turn is done (seq 12)\nsaid: {text}\ncut: 1.0 KB more; \
             agents__transcript(agent: \"child\", seq: \"12\") shows the whole message\n{NEXT}"
        )
    );
}

#[test]
fn stalled() {
    let trajectory = view(Condition::Stalled {
        message: "the trajectory stopped: 12 attempts in a row failed upstream".into(),
        held_triage: false,
    });
    assert_eq!(
        rendered(&trajectory, &driver()),
        "child: stopped: the trajectory stopped: 12 attempts in a row failed upstream\n\
         next: agents__message(agent: \"child\", text: \"\") retries it; agents__message(agent: \
         \"child\", text: \"<message>\") sends a message instead"
    );
    assert_eq!(
        rendered(&trajectory, &observer()),
        "child: stopped: the trajectory stopped: 12 attempts in a row failed upstream"
    );
    let triage = view(Condition::Stalled {
        message: "triage could not reach its model — the results are held until it does".into(),
        held_triage: true,
    });
    assert_eq!(
        rendered(&triage, &driver()),
        "child: stopped: triage could not reach its model — the results are held until it \
         does\nnext: agents__message(agent: \"child\", text: \"\") retries it; a message waits \
         until it clears"
    );
}

#[test]
fn parked_with_a_token_cost() {
    let expired = view(Condition::Parked {
        park: Park::Expired {
            context_tokens: Some(48_210),
        },
    });
    assert_eq!(
        rendered(&expired, &driver()),
        "child: paused before re-reading its context uncached (48k tokens)\n\
         next: agents__message(agent: \"child\", text: \"\") goes ahead and pays that read, as \
         any message does"
    );
    let compacted = view(Condition::Parked {
        park: Park::Compacted,
    });
    assert_eq!(
        rendered(&compacted, &observer()),
        "child: paused; its history was summarized while it waited"
    );
}

/// Neither block is one an agent can lift, so no move is named; a notification of a
/// config repair names the read that shows the parse error, and the read shows it.
#[test]
fn held_by_pace_and_config_repair() {
    let paced = view(Condition::Blocked {
        block: Block::PaceHeld,
    });
    assert_eq!(
        rendered(&paced, &driver()),
        "child: held by its operator; nothing runs until the operator releases the hold"
    );
    let repair = view(Condition::Blocked {
        block: Block::ConfigRepair {
            path: Some("/repo/.efference/child/config.toml".into()),
            parse_error: Some("expected `=` at line 3".into()),
        },
    });
    assert_eq!(
        rendered(&repair, &driver()),
        "child: blocked: its config file needs the operator\n\
         file: /repo/.efference/child/config.toml does not parse\n\
         details: agents__status(agent: \"child\") shows the parse error"
    );
    assert_eq!(
        rendered(&repair, &read(Relation::Driver)),
        "child: blocked: its config file needs the operator\n\
         file: /repo/.efference/child/config.toml does not parse\n\
         error: expected `=` at line 3"
    );
}

/// The MCP mount's table names the same moves by their bare verbs.
#[test]
fn the_mcp_table_names_the_bare_verbs() {
    let reader = Reader::read(Relation::Driver, &Moves::MCP, None);
    let idle = view(Condition::Idle { closing: None });
    assert_eq!(
        rendered(&idle, &reader),
        "child: idle, its turn is done\n\
         next: message(agent: \"child\", text: \"<message>\") sends its next message; \
         archive(agent: \"child\") ends it"
    );
}

/// What a working session is doing, with durations against the reader's clock; the
/// footer is printed for a read and left out of a notification.
#[test]
fn working_with_a_running_bash_command_and_the_footer() {
    let working = StatusView {
        footer: Footer {
            model: Some("anthropic/claude-haiku-5.5".into()),
            context_used: Some(48_210),
            context_limit: Some(1_000_000),
            cost: Some(Cost {
                usd: 0.0421,
                unpriced: 0,
            }),
            background: vec![Background {
                id: 1,
                command: "cargo watch -x test".into(),
                since: NOW - 5 * MINUTE,
            }],
            queued: vec![
                Some("session[parent]".into()),
                None,
                Some("session[parent]".into()),
            ],
            held: false,
        },
        ..view(Condition::Working {
            activity: vec![Activity::Tool {
                tool: "bash".into(),
                command: Some("cargo test".into()),
                since: NOW - 3 * MINUTE - 20 * SECOND,
            }],
        })
    };
    assert_eq!(
        rendered(&working, &read(Relation::Driver)),
        "child: working: running bash `cargo test` for 3m\n\
         anthropic/claude-haiku-5.5 · 48k of 1M context · $0.04 spent\n\
         background: p#1 `cargo watch -x test` for 5m\n\
         queued: 3 messages, from you and operator"
    );
    assert_eq!(
        rendered(&working, &driver()),
        "child: working: running bash `cargo test` for 3m"
    );

    let retrying = view(Condition::Working {
        activity: vec![
            Activity::Thinking {
                since: NOW - 40 * SECOND,
            },
            Activity::Retrying {
                attempt: 2,
                budget: Some(12),
                next_at: Some(NOW + 3 * SECOND),
            },
        ],
    });
    assert_eq!(
        rendered(&retrying, &read(Relation::Observer)),
        "child: working: thinking for 40s\n\
         also: retrying a failed request: attempt 2 of 12 failed, next in 3s"
    );
    assert_eq!(
        rendered(&view(Condition::Working { activity: vec![] }), &driver()),
        "child: working"
    );
}

/// Messages queued behind the operator's hold say so.
#[test]
fn queued_messages_behind_a_hold() {
    let held = StatusView {
        footer: Footer {
            queued: vec![Some("session[parent]".into())],
            held: true,
            ..Footer::default()
        },
        ..view(Condition::Ended)
    };
    assert!(
        rendered(&held, &read(Relation::Driver))
            .contains("queued: 1 message, from you, held by its operator"),
        "{}",
        rendered(&held, &read(Relation::Driver))
    );
}

/// A footer prints only what it knows.
#[test]
fn the_footer_prints_only_its_known_parts() {
    let partial = StatusView {
        footer: Footer {
            context_used: Some(950),
            cost: Some(Cost {
                usd: 1.5,
                unpriced: 2,
            }),
            queued: vec![Some("mcp[claude-code]".into())],
            ..Footer::default()
        },
        ..view(Condition::Ended)
    };
    assert_eq!(
        rendered(&partial, &read(Relation::Observer)),
        "child: ended; its log stays on disk\n950 context · at least $1.50 spent\n\
         queued: 1 message, from claude-code"
    );
    assert_eq!(
        rendered(&view(Condition::Ended), &read(Relation::Driver)),
        "child: ended; its log stays on disk"
    );
}

#[test]
fn ended_failed_and_starting() {
    assert_eq!(
        rendered(&view(Condition::Ended), &driver()),
        "child: ended; its log stays on disk"
    );
    assert_eq!(
        rendered(
            &view(Condition::Failed {
                error: "boom".into()
            }),
            &observer()
        ),
        "child: ended with an error: boom"
    );
    assert_eq!(
        rendered(
            &view(Condition::Starting {
                stage: Stage::Environment,
                authorizing: None,
                since: NOW - 3 * SECOND,
            }),
            &driver()
        ),
        "child: starting, setting up its environment, for 3s"
    );
    assert_eq!(
        rendered(
            &view(Condition::Starting {
                stage: Stage::Preparing,
                authorizing: Some("linear".into()),
                since: NOW - 2 * MINUTE,
            }),
            &driver()
        ),
        "child: starting, waiting for linear to be authorized, for 2m"
    );
    assert_eq!(
        rendered(
            &view(Condition::FailedToStart {
                reason: "env: the env hook exited 7".into()
            }),
            &driver()
        ),
        "child: failed to start: env: the env hook exited 7"
    );
}

/// No line leans on indentation: every line after the first opens with a label.
#[test]
fn every_later_line_opens_with_a_label() {
    let scope = Axes {
        in_scope: 0.21,
        ..axes(0, Some(precedent(0, 0, Some(1))))
    };
    let spot = spot(Origin::Model, "", Some(scope));
    let text = rendered(&gated(&spot, supervisor("parent")), &reviewer());
    for line in text.lines().skip(1) {
        assert_eq!(line, line.trim_start(), "{line:?}");
        let label = line.split(": ").next().unwrap();
        assert!(
            ["intent", "held because", "decide", "escalate"].contains(&label),
            "{line:?}"
        );
    }
}

#[test]
fn durations_and_token_counts() {
    assert_eq!(duration(59 * SECOND), "59s");
    assert_eq!(duration(60 * SECOND), "1m");
    assert_eq!(duration(3 * 60 * MINUTE), "3h");
    assert_eq!(duration(72 * 60 * MINUTE), "3d");
    assert_eq!(tokens_text(950), "950");
    assert_eq!(tokens_text(48_210), "48k");
    assert_eq!(tokens_text(200_000), "200k");
    assert_eq!(tokens_text(1_000_000), "1M");
    assert_eq!(tokens_text(1_240_000), "1.2M");
}
