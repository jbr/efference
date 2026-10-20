use super::super::fixtures::*;
use super::*;
use crate::frontend::Approval;
use crate::log::{Keep, Notification, Stance, Summary, Triaged};

fn child<'a>(me: Option<&'a str>) -> Reading<'a> {
    Reading {
        session: Some("child"),
        me,
        held: None,
    }
}

/// The window `from..=to` of `log`, as `reading` reads it.
fn read(log: &Fixture, from: Seq, to: Seq, reading: &Reading) -> String {
    let entries = log.entries();
    render(entries, &entries[from as usize..=to as usize], reading)
}

fn whole(log: &Fixture, reading: &Reading) -> String {
    let entries = log.entries();
    render(entries, entries, reading)
}

/// Two calls in one turn are `seq.1` and `seq.2`, each one line with its outcome; the
/// spotter's clears, the preamble, a captured result's later verdict and the per-call
/// telemetry render to nothing.
#[test]
fn a_two_call_batch_reads_as_two_lines_with_their_outcomes() {
    let mut log = Fixture::new();
    log.add(Payload::preamble(
        "",
        "You are an agent.",
        Default::default(),
        None,
    ));
    log.user("list src and find WireState", None);
    let list = ToolCall::new("list", "toolu_a", serde_json::json!({ "path": "src" }))
        .with_intent("see the layout");
    let read_call = ToolCall::new(
        "read",
        "toolu_b",
        serde_json::json!({ "paths": ["src/log.rs"] }),
    )
    .with_intent("find WireState");
    log.add(turn("Looking.", vec![list, read_call]));
    log.add(spot("toolu_a", Stance::Clear, "routine (level 0)"));
    log.add(spot("toolu_b", Stance::Clear, "routine (level 0)"));
    log.add(Payload::tool_result(
        "toolu_a",
        RawResult::text("a.rs\nb.rs\nlog.rs\nmain.rs"),
        WireState::Raw,
    ));
    let big = log.add(Payload::tool_result(
        "toolu_b",
        RawResult::text(lines(40)),
        WireState::Pending,
    ));
    log.add(Payload::ResultVerdict {
        for_result: big,
        wire: Triaged::new(Keep::Ranges(vec![(10, 14)]))
            .with_note("the enum")
            .into(),
    });
    log.add(turn("Four variants.", vec![]));
    assert_eq!(
        whole(&log, &child(None)),
        "transcript of child, seq 0–8\n\
         1  operator: list src and find WireState\n\
         2  child: Looking.\n\
         2.1  list `path=\"src\"` — see the layout → ok, 4 lines, it saw all of it\n\
         2.2  read `paths=\"[\\\"src/log.rs\\\"]\"` — find WireState → ok, 40 lines, it saw 5: the enum\n\
         8  child: Four variants.\n"
    );
}

/// A gate decided by the reading parent: an allow names it "you" and the call ran; a
/// deny with a message is that message on the call's line, and the loop's user turn
/// carrying it is not repeated. Another reader sees the parent by its id.
fn gated_by_parent() -> Fixture {
    let mut log = Fixture::new();
    log.user("clean out tmp", Some("session[parent]"));
    log.add(turn(
        "",
        vec![bash("toolu_1", "ls tmp", "see what is there")],
    ));
    log.add(spot("toolu_1", Stance::Gate, "level 2: needs an approval"));
    log.add(decision(
        "toolu_1",
        Approval::Approve,
        Some("session[parent]"),
    ));
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text("a\nb"),
        WireState::Raw,
    ));
    log.add(turn("", vec![bash("toolu_2", "rm -rf tmp", "clear it")]));
    log.add(spot("toolu_2", Stance::Gate, "level 3: destructive"));
    log.add(decision(
        "toolu_2",
        Approval::Redirect("use trash instead".into()),
        Some("session[parent]"),
    ));
    log.add(Payload::tool_result(
        "toolu_2",
        RawResult::error(
            "Not run — parent (agent) replied instead of approving; see the next message.",
        ),
        WireState::Raw,
    ));
    log.steer("use trash instead", Some("session[parent]"));
    log.add(turn("Moving them to the trash.", vec![]));
    log
}

/// The loop's own permission review writes a redirect no person wrote: its decision and
/// its message read as the review's, never as the operator's.
#[test]
fn the_reviews_own_redirect_is_not_the_operators() {
    let mut log = Fixture::new();
    log.user("tidy up", None);
    log.add(turn("", vec![bash("toolu_1", "rm -rf tmp", "clear it")]));
    log.add(spot("toolu_1", Stance::Gate, "level 3: destructive"));
    log.add(decision(
        "toolu_1",
        Approval::Redirect("Gated, and no synchronous reviewer is available.".into()),
        Some("review[autonomous]"),
    ));
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::error("Not run — the permission review declined it; see the next message."),
        WireState::Raw,
    ));
    log.steer("Gated, and no synchronous reviewer is available.", None);
    let read = whole(&log, &child(None));
    assert!(
        read.contains("→ denied by the permission review: Gated, and no synchronous"),
        "{read}"
    );
    assert!(!read.contains("operator: Gated"), "{read}");
}

#[test]
fn a_gate_names_who_allowed_or_denied_it() {
    let log = gated_by_parent();
    assert_eq!(
        whole(&log, &child(Some("parent"))),
        "transcript of child, seq 0–10\n\
         0  you: clean out tmp\n\
         1  bash `ls tmp` — see what is there → allowed by you → ok, 2 lines, it saw all of it\n\
         5  bash `rm -rf tmp` — clear it → denied by you: use trash instead\n\
         10  child: Moving them to the trash.\n"
    );
    assert_eq!(
        whole(&log, &child(Some("mcp[claude-code]"))),
        "transcript of child, seq 0–10\n\
         0  parent: clean out tmp\n\
         1  bash `ls tmp` — see what is there → allowed by parent → ok, 2 lines, it saw all of it\n\
         5  bash `rm -rf tmp` — clear it → denied by parent: use trash instead\n\
         10  child: Moving them to the trash.\n"
    );
}

/// A window that starts after the redirected call shows the decision under the call's
/// handle; one that starts after the decision too shows the message under its decider.
#[test]
fn a_redirect_message_keeps_its_decider_past_the_window_edge() {
    let log = gated_by_parent();
    assert_eq!(
        read(&log, 7, 10, &child(None)),
        "transcript of child, seq 7–10\n\
         5  denied by parent: use trash instead\n\
         10  child: Moving them to the trash.\n"
    );
    assert_eq!(
        read(&log, 9, 10, &child(None)),
        "transcript of child, seq 9–10\n\
         9  parent: use trash instead\n\
         10  child: Moving them to the trash.\n"
    );
}

/// What the session saw: part of a result with the triage's note, none of a result it
/// dropped or kept nothing of, and the waiting states — a call still running, a call
/// held at the gate, a result no verdict has decided yet.
#[test]
fn each_outcome_says_what_the_session_saw() {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![
            bash("toolu_1", "cargo test", "run the tests"),
            bash("toolu_2", "cat big.log", "find the panic"),
            bash("toolu_3", "ls", "look around"),
            bash("toolu_4", "git status", "see what changed"),
        ],
    ));
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text(lines(120)),
        Triaged::new(Keep::Ranges(vec![(1, 3), (7, 7)]))
            .with_note("2 failures")
            .into(),
    ));
    log.add(Payload::tool_result(
        "toolu_2",
        RawResult::error(lines(3)),
        WireState::Elided,
    ));
    log.add(Payload::tool_result(
        "toolu_3",
        RawResult::text(lines(2)),
        Triaged::new(Keep::None)
            .with_note("nothing relevant")
            .into(),
    ));
    log.add(Payload::tool_result(
        "toolu_4",
        RawResult::text(lines(1)),
        WireState::Pending,
    ));
    log.add(turn(
        "",
        vec![
            bash("toolu_5", "cargo build", "build it"),
            bash(
                "toolu_6",
                "curl -sI https://example.com",
                "fetch the headers",
            ),
        ],
    ));
    log.add(Payload::tool_result(
        "toolu_7",
        RawResult::text(""),
        WireState::Raw,
    ));
    let reading = Reading {
        held: Some("toolu_6"),
        ..child(None)
    };
    assert_eq!(
        whole(&log, &reading),
        "transcript of child, seq 0–6\n\
         0.1  bash `cargo test` — run the tests → ok, 120 lines, it saw 4: 2 failures\n\
         0.2  bash `cat big.log` — find the panic → error, 3 lines, it saw none of it\n\
         0.3  bash `ls` — look around → ok, 2 lines, it saw none of it: nothing relevant\n\
         0.4  bash `git status` — see what changed → ok, 1 line, it has not seen it yet\n\
         5.1  bash `cargo build` — build it → running\n\
         5.2  bash `curl -sI https://example.com` — fetch the headers → waiting for a decision\n\
         6  result: ok, no output\n"
    );
}

/// A question reads with its reply right after it, the replier named for the reader;
/// a question an interrupt ended says it was not answered.
#[test]
fn a_question_reads_with_its_reply() {
    let mut log = Fixture::new();
    log.add(turn("Ready.", vec![ask("toolu_a", "Ship it?")]));
    log.submitted(
        Payload::tool_result("toolu_a", RawResult::text("yes, go"), WireState::Raw),
        Some("mcp[claude-code]"),
    );
    log.add(turn(
        "",
        vec![ask("toolu_b", "First?"), ask("toolu_c", "Second?")],
    ));
    log.submitted(
        Payload::tool_result("toolu_b", RawResult::text("no"), WireState::Raw),
        Some("web"),
    );
    log.add(Payload::tool_result(
        "toolu_c",
        RawResult::error("ask interrupted by user"),
        WireState::Raw,
    ));
    assert_eq!(
        whole(&log, &child(Some("mcp[claude-code]"))),
        "transcript of child, seq 0–4\n\
         0  child: Ready.\n\
         0  asked: Ship it?\n\
         1  you: yes, go\n\
         2.1  asked (1 of 2): First?\n\
         3  operator (web): no\n\
         2.2  asked (2 of 2): Second? → not answered\n"
    );
    // The reply read without its question names the question's handle.
    assert_eq!(
        read(&log, 3, 4, &child(None)),
        "transcript of child, seq 3–4\n\
         2.1  answered by operator (web): no\n\
         2.2  not answered\n"
    );
}

/// A notification is a note from its source; a summary is one line naming its range;
/// the title, the kickoff turn and a config edit render to nothing.
#[test]
fn notes_and_summaries_are_one_line_and_mechanism_is_hidden() {
    let mut log = Fixture::new();
    log.user("Introduce yourself.", Some(crate::frontend::KICKOFF_SOURCE));
    log.add(Payload::Title(crate::log::Title {
        text: "Fixing the build".into(),
        author: crate::log::TitleAuthor::Triage,
    }));
    log.add(Payload::Ambient(Notification::new(
        "session[parent]",
        "tests pass on main\n",
        None,
    )));
    log.add(Payload::Summary(Summary::consolidation(
        0,
        2,
        "the opening, summarized",
    )));
    log.add(Payload::ConfigEdit(crate::log::ConfigEdit {
        state: crate::log::EditState::Proposed,
        resolves: None,
        change: crate::log::ConfigChange::Command {
            command: "grep -n".into(),
        },
        note: String::new(),
    }));
    assert_eq!(
        whole(&log, &child(None)),
        "transcript of child, seq 0–4\n\
         2  note from parent: tests pass on main\n\
         3  summarized seq 0–2\n"
    );
    assert_eq!(
        read(&log, 4, 4, &child(None)),
        "transcript of child, seq 4\n(nothing said or done in this range)\n"
    );
    assert_eq!(
        render(log.entries(), &[], &child(None)),
        "transcript of child (no entries)\n"
    );
}

/// A window cut between a call and its result, both ways: the call's line carries an
/// outcome that landed after the window ends; a result, a decision or a verdict whose
/// call is before the window is its own line under the call's handle.
#[test]
fn a_window_cut_between_a_call_and_its_result() {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![
            bash("toolu_1", "cargo test", "run the tests"),
            bash("toolu_2", "ls", "look around"),
        ],
    ));
    log.add(spot("toolu_1", Stance::Gate, "level 2"));
    log.add(decision(
        "toolu_1",
        Approval::Approve,
        Some("session[parent]"),
    ));
    let result = log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text(lines(12)),
        WireState::Pending,
    ));
    log.add(Payload::ResultVerdict {
        for_result: result,
        wire: Triaged::new(Keep::Ranges(vec![(1, 1)]))
            .with_note("1 failure")
            .into(),
    });
    log.add(Payload::tool_result(
        "toolu_2",
        RawResult::text(lines(3)),
        WireState::Raw,
    ));
    assert_eq!(
        read(&log, 0, 1, &child(Some("parent"))),
        "transcript of child, seq 0–1\n\
         0.1  bash `cargo test` — run the tests → allowed by you → ok, 12 lines, it saw 1: 1 failure\n\
         0.2  bash `ls` — look around → ok, 3 lines, it saw all of it\n"
    );
    assert_eq!(
        read(&log, 2, 5, &child(Some("parent"))),
        "transcript of child, seq 2–5\n\
         0.1  allowed by you\n\
         0.1  result: ok, 12 lines, it saw 1: 1 failure\n\
         0.2  result: ok, 3 lines, it saw all of it\n"
    );
    assert_eq!(
        read(&log, 4, 5, &child(None)),
        "transcript of child, seq 4–5\n\
         0.1  verdict: it saw 1 of 12 lines: 1 failure\n\
         0.2  result: ok, 3 lines, it saw all of it\n"
    );
}

/// Every source spelling as a plain name, and "you" for the reader by its label or by
/// the id inside it.
#[test]
fn senders_are_plain_names_for_the_reader() {
    let cases = [
        (None, None, "operator"),
        (Some("web"), None, "operator (web)"),
        (Some("session[parent]"), None, "parent"),
        (Some("session[parent]"), Some("parent"), "you"),
        (Some("session[parent]"), Some("session[parent]"), "you"),
        (Some("mcp[claude-code]"), Some("mcp[claude-code]"), "you"),
        (Some("mcp[claude-code]"), Some("parent"), "claude-code"),
        (Some("agent[old]"), None, "old"),
        (Some("handoff[prior]"), None, "prior (hand-off)"),
        (Some("handoff[prior]"), Some("prior"), "you (hand-off)"),
        (Some("test-watch"), None, "test-watch"),
    ];
    for (source, me, name) in cases {
        assert_eq!(sender(source, me), name, "{source:?} read by {me:?}");
    }
}

/// A continue reads as who sent it, since nothing on the wire says so.
#[test]
fn a_continue_reads_as_its_sender() {
    let mut log = Fixture::new();
    log.user("go on when ready", None);
    log.add(turn("Done.", vec![]));
    log.add(Payload::Telemetry(crate::log::Telemetry::Continue(
        crate::log::ContinueOrigin {
            source: Some("mcp[claude-code]".into()),
        },
    )));
    assert!(whole(&log, &child(None)).contains("2  claude-code continued it\n"));
}

/// An allow's note is on its call's line, and the user turn that carries it is not a
/// line of its own, wherever the window cuts; read alone, the note is an ordinary
/// message from its sender.
#[test]
fn an_allows_note_is_on_the_calls_line() {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![bash("toolu_1", "curl -sI x", "fetch headers")],
    ));
    log.add(allow_noted("toolu_1", Some("session[parent]"), "note-1"));
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text(lines(3)),
        WireState::Raw,
    ));
    log.user_as("use read next time", Some("session[parent]"), "note-1");
    let parent = child(Some("parent"));
    assert_eq!(
        whole(&log, &parent),
        "transcript of child, seq 0–3\n\
         0  bash `curl -sI x` — fetch headers → allowed by you, with \"use read next time\" → ok, 3 lines, it saw all of it\n"
    );
    // The decision in view without its call: the note rides on the decision's line.
    assert_eq!(
        read(&log, 1, 3, &parent),
        "transcript of child, seq 1–3\n\
         0  allowed by you, with \"use read next time\"\n\
         0  result: ok, 3 lines, it saw all of it\n"
    );
    // The note in view alone: nothing on the wire says it is the allow's.
    assert_eq!(
        read(&log, 3, 3, &parent),
        "transcript of child, seq 3\n3  you: use read next time\n"
    );
    // A message from the same sender that no decision names stays its own line.
    let mut other = Fixture::new();
    other.add(turn("", vec![bash("toolu_1", "ls", "look")]));
    other.add(decision(
        "toolu_1",
        Approval::Approve,
        Some("session[parent]"),
    ));
    other.add(Payload::tool_result(
        "toolu_1",
        RawResult::text("a"),
        WireState::Raw,
    ));
    other.user("also, a different thing", Some("session[parent]"));
    assert_eq!(
        whole(&other, &parent),
        "transcript of child, seq 0–3\n\
         0  bash `ls` — look → allowed by you → ok, 1 line, it saw all of it\n\
         3  you: also, a different thing\n"
    );
}

/// A call whose input contains backticks is fenced by a longer run, so the summary is one
/// code span; the status renders it the same way.
#[test]
fn a_summary_with_backticks_is_one_code_span() {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![bash(
            "toolu_1",
            "echo `date` && echo ``x``",
            "show the time",
        )],
    ));
    assert_eq!(
        whole(&log, &child(None)),
        "transcript of child, seq 0\n\
         0  bash ``` echo `date` && echo ``x`` ``` — show the time → running\n"
    );
}
