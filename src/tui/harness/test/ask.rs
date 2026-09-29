//! `ask` in the transcript (docs/tui.md): the question is the assistant's prose with an
//! *asked* mark, the reply is the operator's message, and the composer's send answers it.

use super::*;
use crate::client::WithdrawnTurn;
use crate::tui::parse::PendingAsk;

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
        Some(PendingAsk {
            call_id: "toolu_ask".into(),
            recalled: vec![
                WithdrawnTurn {
                    submission_id: ours.clone(),
                    text: "earlier thought".into(),
                    attachments: Vec::new(),
                },
                WithdrawnTurn {
                    submission_id: "someone-else".into(),
                    text: "not ours".into(),
                    attachments: Vec::new(),
                },
            ],
        })
    };
    play.event(TuiEvent::Ask(topic()));
    assert_eq!(play.state.composer.text(), "earlier thought");
    assert!(play.state.pending_turns.is_empty(), "the echo went with it");

    // The same ask delivered again restores nothing more.
    play.state.composer.clear();
    play.event(TuiEvent::Ask(topic()));
    assert_eq!(play.state.composer.text(), "");
}

/// A client that did not submit the queued turn (a restarted one) restores nothing.
#[test]
fn a_restarted_client_restores_nothing() {
    let mut play = Screenplay::new(70, 12);
    play.event(TuiEvent::Ask(Some(PendingAsk {
        call_id: "toolu_ask".into(),
        recalled: vec![WithdrawnTurn {
            submission_id: "minted-before-restart".into(),
            text: "old".into(),
            attachments: Vec::new(),
        }],
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
