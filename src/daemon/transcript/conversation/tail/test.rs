use super::super::super::fixtures::*;
use super::*;
use crate::log::{Keep, Title, TitleAuthor, Triaged};

fn child() -> Reading<'static> {
    Reading {
        session: Some("child"),
        me: Some("session[parent]"),
        held: None,
    }
}

fn tail(log: &Fixture, n: u64) -> String {
    let entries = log.entries();
    render_tail(entries, entries, n, &child())
}

fn say(log: &mut Fixture, text: &str) -> Seq {
    log.user(text, Some("session[parent]"))
}

/// `count` messages, each followed by a mechanism entry that is no line.
fn chatter(log: &mut Fixture, count: usize) {
    for i in 0..count {
        say(log, &format!("message {i}"));
        log.add(Payload::Title(Title::new(
            format!("title {i}"),
            TitleAuthor::Triage,
        )));
    }
}

/// A short session is shown whole, under the render's plain header.
#[test]
fn a_short_session_shows_all_of_it() {
    let mut log = Fixture::new();
    chatter(&mut log, 3);
    assert_eq!(
        tail(&log, 40),
        "transcript of child, seq 0–5\n\
         0  you: message 0\n\
         2  you: message 1\n\
         4  you: message 2\n"
    );
}

/// A long one is the last n rendered lines: the mechanism entries between them are no
/// line, and the header says how many lines come before.
#[test]
fn a_long_session_shows_the_last_lines_not_the_last_entries() {
    let mut log = Fixture::new();
    chatter(&mut log, 50);
    assert_eq!(
        tail(&log, 5),
        "transcript of child, seq 90–99: the last 5 lines; 45 lines before \
         — since: 0 reads from the start\n\
         90  you: message 45\n\
         92  you: message 46\n\
         94  you: message 47\n\
         96  you: message 48\n\
         98  you: message 49\n"
    );
    assert!(
        tail(&log, 40).starts_with(
            "transcript of child, seq 20–99: the last 40 lines; 10 lines before — since: 0"
        ),
        "{}",
        tail(&log, 40)
    );
    // Nothing asked for shows nothing.
    assert_eq!(tail(&log, 0), "transcript of child (no entries)\n");
}

/// A cut that would put a verdict in the window without its call is extended back to the
/// call, whose line carries the verdict.
#[test]
fn a_cut_does_not_open_on_a_verdict() {
    let mut log = Fixture::new();
    say(&mut log, "run it");
    log.add(turn(
        "",
        vec![bash("toolu_1", "cargo test", "run the tests")],
    ));
    let result = log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text(lines(12)),
        WireState::Pending,
    ));
    say(&mut log, "one");
    say(&mut log, "two");
    log.add(Payload::result_verdict(
        result,
        Triaged::new(Keep::Ranges(vec![(1, 1)]))
            .with_note("1 failure")
            .into(),
    ));
    say(&mut log, "three");
    // The verdict (seq 5) lands before "three", so a window of "three" alone holds none.
    assert_eq!(
        tail(&log, 1),
        "transcript of child, seq 6: the last 1 line; 4 lines before \
         — since: 0 reads from the start\n\
         6  you: three\n"
    );
    // Starting at "two" would hold the verdict without its call: the window opens at the
    // call instead.
    assert_eq!(
        tail(&log, 2),
        "transcript of child, seq 1–6: the last 4 lines; 1 line before \
         — since: 0 reads from the start\n\
         1  bash `cargo test` — run the tests → ok, 12 lines, it saw 1: 1 failure\n\
         3  you: one\n\
         4  you: two\n\
         6  you: three\n"
    );
}

/// A cut between a question and its reply keeps both; a result whose call is further
/// back than the cut pulls the call in too.
#[test]
fn a_cut_keeps_a_question_with_its_reply_and_a_result_with_its_call() {
    let mut log = Fixture::new();
    say(&mut log, "go");
    log.add(turn("", vec![ask("toolu_a", "Which branch?")]));
    log.submitted(
        Payload::tool_result("toolu_a", RawResult::text("main"), WireState::Raw),
        Some("session[parent]"),
    );
    say(&mut log, "thanks");
    say(&mut log, "bye");
    // Lines: go, the question, its reply, thanks, bye. Three is the reply and after.
    assert_eq!(
        tail(&log, 3),
        "transcript of child, seq 1–4: the last 4 lines; 1 line before \
         — since: 0 reads from the start\n\
         1  asked: Which branch?\n\
         2  you: main\n\
         3  you: thanks\n\
         4  you: bye\n"
    );

    let mut log = Fixture::new();
    say(&mut log, "pre");
    log.add(turn("", vec![bash("toolu_1", "sleep 5", "wait")]));
    say(&mut log, "a");
    say(&mut log, "b");
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text("done"),
        WireState::Raw,
    ));
    say(&mut log, "c");
    // Lines: pre, the call (with its result), a, b, c. Two is "b" and after, but the
    // result at seq 4 is in that window, so the call comes with it.
    assert_eq!(
        tail(&log, 2),
        "transcript of child, seq 1–5: the last 4 lines; 1 line before \
         — since: 0 reads from the start\n\
         1  bash `sleep 5` — wait → ok, 1 line, it saw all of it\n\
         2  you: a\n\
         3  you: b\n\
         5  you: c\n"
    );
}
