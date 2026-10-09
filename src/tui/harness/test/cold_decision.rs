//! The cold line at a gate and at an `ask` (docs/tui.md): a decision carries
//! `accept_cold_read` only when the line was on screen to be read.

use super::*;
use crate::frontend::Approval;
use crate::tui::ColdView;
use crate::tui::parse::PendingAsk;

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

fn cold(tokens: Option<u64>) -> TuiEvent {
    TuiEvent::CacheExpired(Some(ColdView {
        context_tokens: tokens,
    }))
}

/// A gate on a cold session states what approving reads, and approving sends the consent.
#[test]
fn a_gate_on_a_cold_session_shows_the_cost_and_approving_consents() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(cold(Some(120_400)));
    assert!(
        play.render()
            .contains("cache believed cold — approving or redirecting reads ~120k tokens uncached"),
        "{}",
        play.render()
    );
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_approval(),
        Some(("toolu_1".to_string(), Approval::Approve, true))
    );
}

/// With no cost reported the line still says the read is uncached.
#[test]
fn the_cold_line_without_a_known_cost() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(cold(None));
    assert!(
        play.render()
            .contains("approving or redirecting reads the context uncached")
    );
}

/// A gate with no cold line is not consent: warm, the decision carries no flag, and
/// once the topic clears the line is gone.
#[test]
fn a_gate_without_the_cold_line_sends_no_consent() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    assert!(!play.render().contains("cache believed cold"));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_approval(),
        Some(("toolu_1".to_string(), Approval::Approve, false))
    );

    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(cold(Some(1_000)));
    play.event(TuiEvent::CacheExpired(None));
    assert!(!play.render().contains("cache believed cold"));
}

/// An abort is not a consent to anything further.
#[test]
fn aborting_a_cold_gate_sends_no_consent() {
    let mut play = Screenplay::new(100, 14);
    gated(&mut play);
    play.event(cold(Some(120_000)));
    play.state.abort_gate();
    assert_eq!(
        play.took_approval(),
        Some(("toolu_1".to_string(), Approval::Abort, false))
    );
}

/// An `ask` on a cold session shows the line, and the answer carries the consent.
#[test]
fn an_answer_to_an_ask_on_a_cold_session_consents() {
    let mut play = Screenplay::new(100, 14);
    play.event(TuiEvent::Ask(Some(PendingAsk {
        call_id: "toolu_ask".into(),
        n: 1,
        of: 1,
        recalled: Vec::new(),
    })));
    play.event(cold(Some(48_000)));
    assert!(
        play.render()
            .contains("cache believed cold — your answer reads ~48k tokens uncached")
    );
    play.type_str("yes");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let sent = play.took_submission().expect("the answer was sent");
    assert_eq!(sent.text, "yes");
    assert!(sent.accept_cold_read);
}

/// With no pending decision there is no cold line: a plain turn on a cold session
/// still earns its `409 cold` and the "⏎ again" consent.
#[test]
fn a_cold_session_with_nothing_pending_shows_no_line() {
    let mut play = Screenplay::new(100, 14);
    play.event(cold(Some(48_000)));
    assert!(!play.render().contains("cache believed cold"));
    play.type_str("hello");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!play.took_submission().unwrap().accept_cold_read);
}
