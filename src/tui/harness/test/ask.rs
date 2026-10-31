//! `ask` in the transcript (docs/tui.md): the question is the assistant's prose with an
//! *asked* mark, the reply is the operator's message, and the composer's send answers it.

use super::*;
use crate::frontend::SubmittedTurn;
use crate::protocol::state::AskView;
use crate::protocol::turns::QueuedTurn;
use crate::tui::harness::asking;

fn ask(id: &str, question: &str) -> ToolCall {
    ToolCall::new("ask", id, json!({ "question": question }))
}

/// A pending question: prose and a mark, none of the call's signature or arguments.
#[test]
fn transcript_ask_pending() {
    let mut play = Screenplay::new(70, 14);
    play.user("Tidy the config loader.");
    play.assistant(
        "The loader has two shapes.",
        vec![ask("toolu_ask", "Keep the legacy `[env]` spelling?")],
    );
    assert!(play.find("asked").is_some());
    assert!(play.find("ask(").is_none(), "the call is not drawn");
    play.snapshot("transcript_ask_pending");
}

/// Answering from the composer: the echo sits after the question until the reply's
/// `ToolResult` (which carries the submission id) settles it, and then reads as an
/// ordinary operator message.
#[test]
fn a_reply_settles_its_echo_and_reads_as_the_operators_message() {
    let mut play = Screenplay::new(70, 14);
    play.assistant("Ready.", vec![ask("toolu_ask", "Ship it?")]);
    play.type_str("yes, ship");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(play.state.pending_turns.len(), 1);
    assert!(play.find("⋯ yes, ship").is_some(), "dimmed while in flight");

    play.commit_reply("toolu_ask", None);
    assert!(play.state.pending_turns.is_empty(), "settled by the result");
    assert!(play.find("⋯").is_none());
    play.snapshot("transcript_ask_answered");
}

/// A reply from a labelled client is attributed as a sourced `User` turn is.
#[test]
fn a_sourced_reply_carries_its_source() {
    let mut play = Screenplay::new(70, 12);
    play.assistant("Ready.", vec![ask("toolu_ask", "Ship it?")]);
    play.type_str("go");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.commit_reply("toolu_ask", Some("web"));
    assert!(play.find("web: go").is_some());
}

/// Several asks in one completion are drawn one after another, each with its place.
#[test]
fn asks_in_a_batch_show_their_position() {
    let mut play = Screenplay::new(70, 14);
    play.assistant(
        "Two things.",
        vec![
            ask("toolu_1", "Which crate?"),
            ask("toolu_2", "Which branch?"),
        ],
    );
    play.snapshot("transcript_ask_batch");
}

/// An interrupt ends the ask with an error result and no submission: the question still
/// reads as asked, and the note says it went unanswered.
#[test]
fn an_unanswered_ask_says_so() {
    let mut play = Screenplay::new(70, 12);
    play.assistant("Ready.", vec![ask("toolu_ask", "Ship it?")]);
    play.tool_result("toolu_ask", "ask interrupted by user", true, WireState::Raw);
    assert!(play.find("Ship it?").is_some());
    assert!(play.find("not answered").is_some());
    assert!(play.find("interrupted").is_none());
}

/// A call whose question streams in is not drawn as a call while it arrives.
#[test]
fn a_streaming_ask_is_not_drawn_as_a_call() {
    let mut play = Screenplay::new(70, 12);
    play.event(TuiEvent::ToolCallStart {
        index: 0,
        call_id: "toolu_ask".into(),
        name: "ask".into(),
    });
    play.event(TuiEvent::ToolCallInput {
        index: 0,
        fragment: r#"{"question":"Ship"#.into(),
    });
    assert!(play.find("calling").is_none());
    assert!(play.find("Ship").is_none());
}

/// Turns queued before the question come back to the composer once, and only the ones
/// this client submitted.
#[test]
fn recalled_turns_return_once_and_only_when_ours() {
    let mut play = Screenplay::new(70, 12);
    play.type_str("earlier thought");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let ours = play.state.pending_turns[0].submission_id.clone();
    let topic = || {
        Some(AskView {
            recalled: vec![
                QueuedTurn::from(&SubmittedTurn::new("earlier thought", ours.clone())),
                QueuedTurn::from(&SubmittedTurn::new("not ours", "someone-else")),
            ],
            ..asking("toolu_ask", 1, 1)
        })
    };
    play.topic(TopicUpdate::Ask(topic()));
    assert_eq!(play.state.composer.text(), "earlier thought");
    assert!(play.state.pending_turns.is_empty(), "the echo went with it");

    // The same ask delivered again restores nothing more.
    play.state.composer.clear();
    play.topic(TopicUpdate::Ask(topic()));
    assert_eq!(play.state.composer.text(), "");
}

/// A client that did not submit the queued turn (a restarted one) restores nothing.
#[test]
fn a_restarted_client_restores_nothing() {
    let mut play = Screenplay::new(70, 12);
    play.topic(TopicUpdate::Ask(Some(AskView {
        recalled: vec![QueuedTurn::from(&SubmittedTurn::new(
            "old",
            "minted-before-restart",
        ))],
        ..asking("toolu_ask", 1, 1)
    })));
    assert_eq!(play.state.composer.text(), "");
}

/// The daemon's refusal to continue past a question reaches the status line as it does
/// for any other refusal.
#[test]
fn the_continue_refusal_reaches_the_status_line() {
    let mut play = Screenplay::new(80, 10);
    play.event(TuiEvent::Notice(
        "a question is pending — answer it with a turn to continue".into(),
    ));
    assert!(play.find("a question is pending").is_some());
}

fn topic(call_id: &str, n: usize, of: usize) -> TuiEvent {
    TuiEvent::Topic(TopicUpdate::Ask(Some(asking(call_id, n, of))))
}

/// Two questions in one completion: the topic names the first, which the composer's
/// send answers; the second is marked as coming next, and the status line says which.
#[test]
fn transcript_ask_batch_first_current() {
    let mut play = Screenplay::new(70, 14);
    play.assistant(
        "Two things.",
        vec![
            ask("toolu_1", "Which crate?"),
            ask("toolu_2", "Which branch?"),
        ],
    );
    play.event(topic("toolu_1", 1, 2));
    assert!(play.find("▸ answering · question 1 of 2").is_some());
    assert!(play.find("next · question 2 of 2").is_some());
    assert!(play.find("your reply answers question 1 of 2").is_some());
    play.snapshot("transcript_ask_batch_first_current");
}

/// The first is answered and the topic moves to the second: the mark on the second
/// changes with no log entry behind it, and the first reads as asked with its reply.
#[test]
fn transcript_ask_batch_second_current() {
    let mut play = Screenplay::new(70, 16);
    play.assistant(
        "Two things.",
        vec![
            ask("toolu_1", "Which crate?"),
            ask("toolu_2", "Which branch?"),
        ],
    );
    play.event(topic("toolu_1", 1, 2));
    play.type_str("efference");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.commit_reply("toolu_1", None);
    play.event(topic("toolu_2", 2, 2));
    assert!(play.find("asked · question 1 of 2").is_some());
    assert!(play.find("▸ answering · question 2 of 2").is_some());
    assert!(play.find("next").is_none());
    play.snapshot("transcript_ask_batch_second_current");
}

/// A lone pending question: emphasized, without a position.
#[test]
fn transcript_ask_lone_current() {
    let mut play = Screenplay::new(70, 12);
    play.assistant("Ready.", vec![ask("toolu_ask", "Ship it?")]);
    play.event(topic("toolu_ask", 1, 1));
    assert!(play.find("▸ answering").is_some());
    assert!(play.find("your reply answers the question").is_some());
    play.snapshot("transcript_ask_lone_current");
}

/// The topic clearing (the ask ended some other way) leaves an unresolved question as
/// merely asked: nothing observable says it is still open.
#[test]
fn a_cleared_topic_leaves_an_ask_as_asked() {
    let mut play = Screenplay::new(70, 12);
    play.assistant("Ready.", vec![ask("toolu_ask", "Ship it?")]);
    play.event(topic("toolu_ask", 1, 1));
    play.topic(TopicUpdate::Ask(None));
    assert!(play.find("▸ answering").is_none());
    assert!(play.find("asked").is_some());
    assert!(play.find("your reply answers").is_none());
}

/// The mark is the emphasized register only while current.
#[test]
fn the_current_mark_is_bolder_than_the_upcoming_one() {
    let mut play = Screenplay::new(70, 14);
    play.assistant(
        "Two things.",
        vec![
            ask("toolu_1", "Which crate?"),
            ask("toolu_2", "Which branch?"),
        ],
    );
    play.event(topic("toolu_1", 1, 2));
    let (cx, cy) = play.find("answering").expect("current mark");
    let (ux, uy) = play.find("next").expect("upcoming mark");
    let current = play.cell(cx, cy);
    let upcoming = play.cell(ux, uy);
    assert!(
        current.modifier.contains(Modifier::BOLD) || current.fg != upcoming.fg,
        "current {current:?} vs upcoming {upcoming:?}"
    );
}

/// A send typed before the question arrived names none and is refused: its text comes back
/// to the composer, its echo goes, and the status line says why.
#[test]
fn a_send_refused_for_a_pending_question_returns_to_the_composer() {
    let mut play = Screenplay::new(80, 12);
    play.type_str("keep going");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let sent = play.took_submission().expect("sent");
    assert_eq!(sent.answers, None, "no question was showing");
    assert_eq!(play.state.pending_turns.len(), 1);

    play.event(TuiEvent::NotDelivered {
        turn: QueuedTurn::from(&sent.turn),
        answered: false,
    });
    assert_eq!(play.state.composer.text(), "keep going");
    assert!(play.state.pending_turns.is_empty(), "the echo is gone");
    assert!(play.render().contains("a question came first"));
}

/// A send while a question shows names it.
#[test]
fn a_send_while_a_question_shows_names_it() {
    let mut play = Screenplay::new(70, 12);
    play.topic(TopicUpdate::Ask(Some(asking("toolu_ask", 1, 1))));
    play.type_str("yes");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let sent = play.took_submission().expect("sent");
    assert_eq!(sent.answers.as_deref(), Some("toolu_ask"));
}
