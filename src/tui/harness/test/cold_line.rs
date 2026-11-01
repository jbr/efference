//! The cold line at a gate, at an `ask` and at the composer (docs/tui.md): it states
//! what the next action reads uncached, and the action goes through unchanged.

use super::*;
use crate::record::log::Approval;
use crate::tui::harness::{asking, cold};

fn gated(play: &mut Screenplay) {
    play.user("Clean the build directory.");
    play.assistant(
        "Sure.",
        vec![ToolCall::new(
            "bash",
            "toolu_1",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate("toolu_1", "bash", json!({ "command": "rm -rf ./build" }));
}

fn expired(tokens: Option<u64>) -> TuiEvent {
    TuiEvent::Topic(TopicUpdate::Cold(Some(cold(tokens))))
}

/// A gate on a cold session states what approving reads, and approving sends the decision.
#[test]
fn a_gate_on_a_cold_session_shows_the_cost_and_approving_decides() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(expired(Some(120_400)));
    assert!(
        play.render()
            .contains("cache expired — approving or redirecting reads ~120k tokens uncached"),
        "{}",
        play.render()
    );
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_approval(),
        Some(("toolu_1".to_string(), Approval::Approve))
    );
}

/// With no cost reported the line still says the read is uncached.
#[test]
fn the_cold_line_without_a_known_cost() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(expired(None));
    assert!(
        play.render()
            .contains("approving or redirecting reads the context uncached")
    );
}

/// A warm gate has no cold line, and once the topic clears the line is gone.
#[test]
fn a_gate_without_the_cold_line() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    assert!(!play.render().contains("cache expired"));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_approval(),
        Some(("toolu_1".to_string(), Approval::Approve))
    );

    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(expired(Some(1_000)));
    play.topic(TopicUpdate::Cold(None));
    assert!(!play.render().contains("cache expired"));
}

/// An abort on a cold gate is sent as it is.
#[test]
fn aborting_a_cold_gate() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(expired(Some(120_000)));
    play.state.abort_gate();
    assert_eq!(
        play.took_approval(),
        Some(("toolu_1".to_string(), Approval::Abort))
    );
}

/// An `ask` on a cold session shows the line, and the answer is a turn naming the
/// question it answers.
#[test]
fn an_answer_to_an_ask_on_a_cold_session_shows_the_cost() {
    let mut play = Screenplay::new(100, 14);
    play.topic(TopicUpdate::Ask(Some(asking("toolu_ask", 1, 1))));
    play.event(expired(Some(48_000)));
    assert!(
        play.render()
            .contains("cache expired — your answer reads ~48k tokens uncached")
    );
    play.type_str("yes");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let sent = play.took_submission().expect("the answer was sent");
    assert_eq!(sent.turn.text, "yes");
    assert_eq!(sent.answers.as_deref(), Some("toolu_ask"));
}

/// With nothing pending the composer states what a send reads, and a send goes
/// through on the first `⏎`.
#[test]
fn the_composer_on_a_cold_session_shows_the_cost_and_sends() {
    let mut play = Screenplay::new(100, 14);
    play.event(expired(Some(48_000)));
    assert!(
        play.render()
            .contains("cache expired — a send reads ~48k tokens uncached"),
        "{}",
        play.render()
    );
    play.type_str("hello");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let sent = play.took_submission().unwrap();
    assert_eq!(sent.turn.text, "hello");
    assert_eq!(sent.answers, None, "no question was showing");
}
