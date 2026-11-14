use super::*;
use crate::clients::tui::harness::{gate, process};
use crate::clients::tui::process_panel::TAIL_CAP;
use crate::clients::tui::render_context::RenderContext;
use crate::clients::tui::topics::TopicUpdate;
use tokio::sync::mpsc::UnboundedReceiver;

/// The `kind` frame carrying `value`, read the way the bridge reads it.
fn frame(kind: &str, value: serde_json::Value) -> TuiEvent {
    match TopicUpdate::decode(kind, value) {
        Ok(Some(update)) => TuiEvent::Topic(update),
        other => panic!("not a {kind} frame: {other:?}"),
    }
}

/// A state with one pending gate, plus the receiver the decision lands on.
fn state_with_gate() -> (
    InteractiveState<'static>,
    UnboundedReceiver<(String, Approval)>,
) {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    let arx = receivers.approve;
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.topics.gate = Some(gate(
        "toolu_1",
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    (s, arx)
}

#[test]
fn empty_submit_approves_the_gate() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer(String::new());
    let (id, dec) = arx.try_recv().expect("a decision was sent");
    assert_eq!(id, "toolu_1");
    assert!(matches!(dec, Approval::Approve));
    assert!(s.topics.gate.is_none(), "gate cleared after resolving");
}

#[test]
fn other_text_redirects_the_gate() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer("use rg instead".into());
    match arx.try_recv().unwrap().1 {
        Approval::Redirect(m) => assert_eq!(m, "use rg instead"),
        other => panic!("expected redirect, got {other:?}"),
    }
}

/// The composer composes text only: the old `.abort` / `.allow` sentinels redirect
/// with their literal text, as any other text does (docs/tui.md).
#[test]
fn former_sentinels_redirect_with_their_literal_text() {
    for text in [".abort", ".allow", ".allow git log"] {
        let (mut s, mut arx) = state_with_gate();
        s.submit_composer(text.into());
        match arx.try_recv().unwrap().1 {
            Approval::Redirect(m) => assert_eq!(m, text),
            other => panic!("expected redirect for {text:?}, got {other:?}"),
        }
        assert!(s.topics.gate.is_none(), "gate cleared after resolving");
    }
}

#[test]
fn process_output_smooths_the_matching_process_tail() {
    let (mut s, _arx) = state_with_gate();
    s.handle_event(TuiEvent::Topic(TopicUpdate::Processes(vec![process(
        1,
        "cargo watch",
    )])));
    s.handle_event(TuiEvent::ProcessOutput {
        id: 1,
        chunk: "tick 1\n".into(),
    });
    s.dirty = false;
    // A delta for the live process appends to its tail.
    s.handle_event(TuiEvent::ProcessOutput {
        id: 1,
        chunk: "tick 2\n".into(),
    });
    assert_eq!(s.process_panel[0].tail, "tick 1\ntick 2\n");
    assert!(s.dirty, "panel repaints");
    // A delta for an unknown id is ignored (it appears with the next state change).
    s.handle_event(TuiEvent::ProcessOutput {
        id: 9,
        chunk: "nope".into(),
    });
    assert_eq!(s.process_panel[0].tail, "tick 1\ntick 2\n");
}

#[test]
fn process_output_bounds_the_tail_on_a_char_boundary() {
    let (mut s, _arx) = state_with_gate();
    s.handle_event(TuiEvent::Topic(TopicUpdate::Processes(vec![process(
        1, "chatty",
    )])));
    // A multibyte burst past the cap must not panic and must stay valid UTF-8.
    let chunk = "é".repeat(TAIL_CAP); // 2 bytes each → 2× the cap
    s.handle_event(TuiEvent::ProcessOutput { id: 1, chunk });
    assert!(s.process_panel[0].tail.len() <= TAIL_CAP);
    assert!(
        s.process_panel[0].tail.chars().all(|c| c == 'é'),
        "no codepoint sliced"
    );
}

#[test]
fn submit_without_gate_is_a_normal_turn() {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    let (mut srx, mut arx) = (receivers.submit, receivers.approve);
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.submit_composer("hello".into());
    assert_eq!(
        srx.try_recv().unwrap().turn.readable(),
        "hello",
        "went to the turn channel"
    );
    assert!(arx.try_recv().is_err(), "not to the approval channel");
}

/// The paste threshold (docs/inbox.md): a short paste goes into the composer; a long one is
/// staged — uploaded at once, the composer untouched — and the next send carries it once the
/// upload has landed. A send while it is still uploading waits rather than dropping it.
#[test]
fn a_long_paste_is_staged_and_sent_with_the_next_turn() {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    let (mut srx, mut atx) = (receivers.submit, receivers.attachments);
    let mut s = InteractiveState::new(erx, senders, rc, theme);

    let short = "one\ntwo\nthree";
    s.handle_paste(short);
    assert_eq!(s.composer.text(), short, "a short paste inlines");

    let long = (0..40)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    s.handle_paste(&long);
    assert_eq!(s.composer.text(), short, "the composer is untouched");
    let crate::clients::tui::staged::AttachCmd::Upload { key, name, bytes } =
        atx.try_recv().unwrap()
    else {
        panic!("a long paste uploads")
    };
    assert!(
        name.starts_with("paste-") && name.ends_with(".txt"),
        "{name}"
    );
    assert_eq!(bytes, long.as_bytes());

    s.submit_composer("look".into());
    assert!(srx.try_recv().is_err(), "a send waits for its upload");

    let arrival = crate::record::log::attachment::Arrival {
        name: name.clone(),
        path: format!("att/composer/{name}"),
        kind: crate::record::log::attachment::Kind::Binary { bytes: 1 },
    };
    s.upload_landed(key, Ok(arrival.clone()));
    s.submit_composer("look".into());
    let sent = srx.try_recv().unwrap();
    assert_eq!(sent.turn.attachment_arrivals(), vec![arrival]);
    assert!(s.staged.is_empty(), "sent chips leave the row");
}

/// `C-o` asks for a compaction (backlog: cold-session.md).
#[test]
fn ctrl_o_asks_for_a_fold() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    let mut frx = receivers.compaction;
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.handle_composer_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(frx.try_recv().is_ok(), "a compaction was asked for");
}

/// The operation text shows only while the loop is running, follows the topic, and
/// clears with it (docs/tui.md): an idle session's keepalive is not a wait.
#[test]
fn the_operation_text_follows_the_topic_while_running() {
    let (mut s, _arx) = state_with_gate();
    let now = crate::record::log::Timestamp::now().0;
    let topic = serde_json::json!({
        "requests": [{"request": 0, "origin": "turn", "stage": "sent", "since": now}],
        "tools": [],
    });

    assert!(!s.handle_event(frame("current_operations", topic)));
    assert_eq!(s.operation_text, None, "not shown while the loop is idle");

    assert!(!s.handle_event(TuiEvent::Topic(TopicUpdate::LoopPhase(
        crate::protocol::state::LoopPhase::Requesting
    ))));
    assert_eq!(
        s.operation_text.as_deref(),
        Some("waiting on the model · 0s")
    );

    assert!(!s.handle_event(TuiEvent::Topic(TopicUpdate::CurrentOperations(None))));
    assert_eq!(s.operation_text, None);
}

/// The gated (or asking) phase owns the status bar's operation slot: a side request
/// open beside it is not what the operator is being waited on for (docs/tui.md).
#[test]
fn a_pending_gate_or_ask_leads_the_operation_text() {
    let (mut s, _arx) = state_with_gate();
    let now = crate::record::log::Timestamp::now().0;
    let topic = serde_json::json!({
        "requests": [{"request": 0, "origin": "title", "stage": "sent", "since": now}],
        "tools": [],
    });
    s.handle_event(TuiEvent::Topic(TopicUpdate::LoopPhase(
        crate::protocol::state::LoopPhase::Gated,
    )));
    s.handle_event(frame("current_operations", topic));
    assert!(s.topics.gate.is_some());
    assert_eq!(
        s.status_operation_text().as_deref(),
        Some("waiting for your approval")
    );

    s.topics.gate = None;
    s.handle_event(TuiEvent::Topic(TopicUpdate::LoopPhase(
        crate::protocol::state::LoopPhase::Requesting,
    )));
    assert_eq!(
        s.status_operation_text().as_deref(),
        Some("writing a title · 0s")
    );
}

/// A state whose submission receiver is kept, for asserting what a send
/// carried.
fn sendable_state() -> (
    InteractiveState<'static>,
    UnboundedReceiver<crate::clients::tui::senders::Submission>,
) {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    let s = InteractiveState::new(erx, senders, rc, theme);
    (s, receivers.submit)
}

use crate::protocol::turns::QueuedTurn;
use crate::record::log::{ReferenceDefinition, UserSegment};
use std::collections::BTreeMap;

/// A document of one prose piece and one agent occurrence.
fn review_document(prose: &str) -> UserContent {
    UserContent {
        content: vec![
            UserSegment::Text(prose.into()),
            UserSegment::Reference("a".into()),
        ],
        references: BTreeMap::from([(
            "a".into(),
            ReferenceDefinition::agent("reviewer", "castback"),
        )]),
    }
}

/// A withdrawn turn in the shape a recall carries.
fn queued(document: UserContent, id: &str) -> QueuedTurn {
    QueuedTurn::from(&SubmittedTurn::structured(document, id))
}

fn attachment(name: &str) -> crate::record::log::attachment::Arrival {
    crate::record::log::attachment::Arrival {
        name: name.into(),
        path: format!("att/composer/{name}"),
        kind: crate::record::log::attachment::Kind::Binary { bytes: 3 },
    }
}

/// A recall of a documented turn plus a plain one, with a draft underneath:
/// every turn contributes its document or text as the readable form, and the
/// merged document equals the buffer.
#[test]
fn a_mixed_recall_over_a_draft_holds_the_whole_buffer() {
    let (mut s, _srx) = sendable_state();
    s.composer.set_text("my draft");
    let documented = queued(review_document("review "), "s1");
    let plain = QueuedTurn::from(&SubmittedTurn::new("second", "s2"));
    s.apply_recall(&[documented, plain]);
    let expected = "review @castback\nsecond\nmy draft";
    assert_eq!(s.composer.text(), expected);
    let (document, spliced) = s.recalled_document.as_ref().expect("held");
    assert_eq!(spliced, expected);
    assert_eq!(document.readable(), expected);
    assert!(document.references.contains_key("a"));
}

/// A second recall while the held text has been edited puts the held spelling
/// back — references travel only with it — and keeps the first turn's
/// references in the merged document. The notice says so; the recalled text
/// stays in the composer, so no turn is lost.
#[test]
fn a_second_recall_reverts_an_edited_hold_and_keeps_its_references() {
    let (mut s, mut srx) = sendable_state();
    s.apply_recall(&[queued(review_document("review "), "s1")]);
    assert_eq!(s.composer.text(), "review @castback");
    s.composer.set_text("review @castback thoroughly");
    s.apply_recall(&[QueuedTurn::from(&SubmittedTurn::new("more", "s2"))]);
    let expected = "more\nreview @castback";
    assert_eq!(s.composer.text(), expected, "the held spelling is back");
    assert!(
        s.ui.debug_message.contains("back as recalled"),
        "the operator is told the edit was put back: {}",
        s.ui.debug_message
    );
    let (document, spliced) = s.recalled_document.as_ref().expect("held");
    assert_eq!(spliced, expected);
    assert!(document.references.contains_key("a"));
    assert_eq!(document.readable(), expected);
    s.submit_composer(expected.into());
    let sent = srx.try_recv().expect("the reverted send goes out");
    let document = sent.turn.document().expect("structured");
    assert!(document.references.contains_key("a"));
    assert_eq!(document.readable(), expected);
}

/// An attachment-only hold has no readable form of its own, and a second
/// recall must still carry its document: the buffer is the new text alone,
/// with no separator, and the attachment is still in the merged document.
#[test]
fn a_second_recall_keeps_an_attachment_only_hold() {
    let (mut s, mut srx) = sendable_state();
    let documented = UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    s.apply_recall(&[queued(documented, "s1")]);
    assert_eq!(s.composer.text(), "");
    assert!(s.recalled_document.is_some(), "held while empty");
    s.apply_recall(&[QueuedTurn::from(&SubmittedTurn::new("more", "s2"))]);
    assert_eq!(
        s.composer.text(),
        "more",
        "the empty hold adds no separator"
    );
    let (document, spliced) = s.recalled_document.as_ref().expect("held");
    assert_eq!(spliced, "more");
    assert_eq!(document.attachment_arrivals().len(), 1);
    assert_eq!(document.readable(), "more");
    s.submit_composer("more".into());
    let sent = srx.try_recv().expect("sent");
    let document = sent.turn.document().expect("structured");
    assert_eq!(document.attachment_arrivals().len(), 1);
    assert_eq!(document.readable(), "more");
}

/// Two withdrawn turns can carry the same upload — a refusal and a resend both
/// recalled, for instance. The merge keeps one definition and one occurrence:
/// the gate admits one definition per upload, and an attachment spells
/// nothing, so the readable form does not change.
#[test]
fn a_recall_of_two_turns_with_the_same_upload_references_it_once() {
    let (mut s, mut srx) = sendable_state();
    let same_upload = || UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    s.apply_recall(&[queued(same_upload(), "s1"), queued(same_upload(), "s2")]);
    let (document, spliced) = s.recalled_document.as_ref().expect("held");
    assert_eq!(spliced, "\n", "two zero-width turns joined as two blocks");
    assert_eq!(document.references.len(), 1, "one upload, one definition");
    assert_eq!(
        document
            .content
            .iter()
            .filter(|segment| matches!(segment, UserSegment::Reference(_)))
            .count(),
        1,
        "one occurrence"
    );
    assert_eq!(document.readable(), *spliced);
    s.submit_composer(spliced.clone());
    let sent = srx.try_recv().expect("sent");
    let document = sent.turn.document().expect("structured");
    assert_eq!(document.references.len(), 1);
}

/// Editing a held draft is refused once, keeps the hold, and the unedited
/// resend still goes out structured.
#[test]
fn an_edit_is_refused_and_the_unedited_resend_keeps_structure() {
    let (mut s, mut srx) = sendable_state();
    s.apply_recall(&[queued(review_document("review "), "s1")]);
    s.composer.set_text("review @castback edited");
    s.submit_composer("review @castback edited".into());
    assert_eq!(
        s.composer.text(),
        "review @castback",
        "recalled text restored"
    );
    assert!(
        s.recalled_document.is_some(),
        "the hold survives the refusal"
    );
    assert!(srx.try_recv().is_err(), "nothing was sent");
    s.submit_composer("review @castback".into());
    let sent = srx.try_recv().expect("sent after the refusal");
    let document = sent.turn.document().expect("structured");
    assert_eq!(document.readable(), "review @castback");
    assert!(document.references.contains_key("a"));
    assert!(matches!(
        sent.turn.payload(),
        Payload::User(blocks)
            if blocks.len() == 1
                && matches!(blocks[0], crate::record::log::ContentBlock::Document(_))
    ));
}

/// Sending an edited attachment-only hold is refused, its empty spelling goes
/// back, and the hold survives so the next send still carries the upload.
#[test]
fn an_edited_attachment_only_hold_survives_its_refusal() {
    let (mut s, mut srx) = sendable_state();
    let document = UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    s.apply_recall(&[queued(document, "s1")]);
    s.submit_composer("typed over the hold".into());
    assert!(srx.try_recv().is_err(), "nothing was sent");
    assert_eq!(s.composer.text(), "", "the empty spelling is back");
    assert!(
        s.recalled_document.is_some(),
        "the hold survives the refusal"
    );
    assert!(
        s.ui.debug_message.contains("send it unedited"),
        "the operator is told: {}",
        s.ui.debug_message
    );
    s.submit_composer(String::new());
    let sent = srx.try_recv().expect("the unedited send goes out");
    let document = sent.turn.document().expect("structured");
    assert_eq!(document.attachment_arrivals().len(), 1);
}

/// Removing a recalled attachment detaches it from the held document; other
/// definitions keep the hold alive.
#[test]
fn removing_one_attachment_keeps_the_rest_of_the_hold() {
    let (mut s, _srx) = sendable_state();
    let mut document = review_document("look ");
    document.content.push(UserSegment::Reference("r1".into()));
    document.references.insert(
        "r1".into(),
        ReferenceDefinition::attachment(attachment("shot.png")),
    );
    s.apply_recall(&[queued(document, "s1")]);
    assert_eq!(s.staged.items().len(), 1);
    s.remove_staged(s.staged.items()[0].key);
    let (held, _) = s
        .recalled_document
        .as_ref()
        .expect("the agent reference remains");
    assert!(!held.references.contains_key("r1"));
    assert!(held.references.contains_key("a"));
    assert_eq!(held.readable(), "look @castback");
}

/// An attachment-only hold has no readable form of its own; removing its chip
/// leaves no definition, so the hold goes and the text becomes editable.
#[test]
fn removing_the_last_attachment_drops_the_hold() {
    let (mut s, _srx) = sendable_state();
    let document = UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    s.apply_recall(&[queued(document, "s1")]);
    assert_eq!(s.composer.text(), "");
    assert!(
        s.recalled_document.is_some(),
        "an attachment-only hold starts empty"
    );
    s.remove_staged(s.staged.items()[0].key);
    assert!(s.recalled_document.is_none());
}

/// Clearing the composer is the explicit way out: an edit that empties a
/// non-empty buffer drops the hold, while an attachment-only splice that
/// starts empty does not clear itself.
#[test]
fn clearing_the_composer_drops_an_attachment_only_hold() {
    use crate::clients::tui::harness::Screenplay;
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut play = Screenplay::new(80, 24);
    let document = UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    play.state.apply_recall(&[queued(document, "s1")]);
    assert_eq!(play.state.composer.text(), "");
    assert!(play.state.recalled_document.is_some(), "held while empty");
    play.route_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(play.state.recalled_document.is_some(), "typing keeps it");
    play.route_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(play.state.composer.text(), "");
    assert!(
        play.state.recalled_document.is_none(),
        "empty is the way out"
    );
}

/// A successful structured send clears the hold, as it clears the staged
/// chips: the next text is an ordinary turn, not a resubmission refused
/// against the spelling the document it sent had.
#[test]
fn a_successful_send_clears_the_hold_and_the_next_text_is_an_ordinary_turn() {
    let (mut s, mut srx) = sendable_state();
    s.apply_recall(&[queued(review_document("review "), "s1")]);
    s.submit_composer("review @castback".into());
    let sent = srx.try_recv().expect("the recalled turn goes out");
    assert_eq!(
        sent.turn.document().map(|document| document.readable()),
        Some("review @castback".to_string())
    );
    assert!(s.recalled_document.is_none(), "the send clears the hold");
    s.composer.set_text("next");
    s.submit_composer("next".into());
    let sent = srx.try_recv().expect("the ordinary turn goes out");
    assert_eq!(sent.turn.readable(), "next");
    assert!(
        sent.turn.document().is_none(),
        "plain text, no document and no references"
    );
    // Every submission lands as one `Document` block; a plain turn's document
    // has no references.
    assert!(matches!(
        sent.turn.payload(),
        Payload::User(blocks)
            if blocks.len() == 1
                && matches!(
                    &blocks[0],
                    crate::record::log::ContentBlock::Document(content) if content.is_plain()
                )
    ));
}

/// A send that never left keeps its hold and its text: nothing was submitted,
/// so the next attempt must still be the structured one.
#[test]
fn a_failed_send_keeps_the_hold() {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    // No receiver: the send reports the daemon as gone.
    drop(receivers);
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.apply_recall(&[queued(review_document("review "), "s1")]);
    s.submit_composer("review @castback".into());
    assert!(
        s.recalled_document.is_some(),
        "nothing went out, so the hold stays"
    );
    assert_eq!(
        s.composer.text(),
        "review @castback",
        "the text stays for a retry"
    );
    assert!(
        s.ui.debug_message.contains("could not send"),
        "the failure is stated: {}",
        s.ui.debug_message
    );
}

/// The same for an attachment-only hold, whose readable form is empty: left
/// in place, it would make the next send carry an upload no chip shows.
#[test]
fn an_attachment_only_send_clears_its_hold() {
    let (mut s, mut srx) = sendable_state();
    let document = UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    s.apply_recall(&[queued(document, "s1")]);
    s.submit_composer(String::new());
    let sent = srx.try_recv().expect("the attachment-only turn goes out");
    assert_eq!(
        sent.turn
            .document()
            .map(|document| document.attachment_arrivals().len()),
        Some(1)
    );
    assert!(s.recalled_document.is_none(), "the send clears the hold");
    s.composer.set_text("plain");
    s.submit_composer("plain".into());
    let sent = srx.try_recv().expect("the ordinary turn goes out");
    assert_eq!(sent.turn.readable(), "plain");
    assert!(sent.turn.document().is_none(), "no stale attachment");
    assert!(sent.turn.attachment_arrivals().is_empty());
}

/// A gate resolves with text, which has no place for a reference definition:
/// a held document is refused rather than flattened into a redirect with its
/// references dropped without a trace. Approving or aborting from the strip
/// does not touch the composer or its hold, so the turn stays ready to send.
#[test]
fn a_gate_does_not_take_a_held_document_as_its_answer() {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::clients::tui::senders::Senders::channels();
    let (mut srx, mut arx) = (receivers.submit, receivers.approve);
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.topics.gate = Some(gate(
        "toolu_1",
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    s.apply_recall(&[queued(review_document("review "), "s1")]);
    let held = s.composer.text().to_string();
    s.submit_composer("use rg instead".into());
    assert!(arx.try_recv().is_err(), "nothing resolved the gate");
    assert!(s.topics.gate.is_some(), "the gate is still pending");
    assert!(s.recalled_document.is_some(), "the hold is intact");
    assert_eq!(s.composer.text(), held, "the spelling is untouched");
    assert!(
        s.ui.debug_message.contains("strip"),
        "the operator is pointed at the strip verbs: {}",
        s.ui.debug_message
    );
    s.approve_gate();
    assert!(matches!(arx.try_recv().unwrap().1, Approval::Approve));
    assert!(s.topics.gate.is_none(), "the strip resolved the gate");
    assert!(s.recalled_document.is_some(), "approving keeps the hold");
    assert_eq!(s.composer.text(), held);
    s.submit_composer(held);
    let sent = srx.try_recv().expect("the held turn goes out afterwards");
    assert_eq!(
        sent.turn.document().map(|document| document.readable()),
        Some("review @castback".to_string())
    );
}

/// An attachment-only hold's readable form is empty, and an empty composer at
/// a gate is an approve: that spelling must not quietly resolve the gate and
/// lose the upload either.
#[test]
fn an_attachment_only_hold_does_not_approve_a_gate() {
    let (mut s, mut arx) = state_with_gate();
    let document = UserContent {
        content: vec![UserSegment::Reference("r1".into())],
        references: BTreeMap::from([(
            "r1".into(),
            ReferenceDefinition::attachment(attachment("shot.png")),
        )]),
    };
    s.apply_recall(&[queued(document, "s1")]);
    assert_eq!(s.composer.text(), "");
    s.submit_composer(String::new());
    assert!(arx.try_recv().is_err(), "nothing approved the gate");
    assert!(s.topics.gate.is_some(), "the gate is still pending");
    assert!(s.recalled_document.is_some(), "the upload is still held");
    assert!(
        s.ui.debug_message.contains("strip"),
        "the operator is pointed at the strip verbs: {}",
        s.ui.debug_message
    );
}
