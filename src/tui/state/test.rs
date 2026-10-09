use super::*;
use crate::tui::render_context::RenderContext;
use tokio::sync::mpsc::UnboundedReceiver;

/// A state with one pending gate, plus the receiver the decision lands on.
fn state_with_gate() -> (
    InteractiveState<'static>,
    UnboundedReceiver<(String, Approval, bool)>,
) {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::tui::senders::Senders::channels();
    let arx = receivers.approve;
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.pending_gate = Some(PendingGate {
        call_id: "toolu_1".into(),
        name: "bash".into(),
        input: serde_json::json!({ "command": "ls" }),
        reason: None,
        acting_on: None,
    });
    (s, arx)
}

#[test]
fn empty_submit_approves_the_gate() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer(String::new());
    let (id, dec, _) = arx.try_recv().expect("a decision was sent");
    assert_eq!(id, "toolu_1");
    assert!(matches!(dec, Approval::Approve));
    assert!(s.pending_gate.is_none(), "gate cleared after resolving");
}

#[test]
fn abort_sentinel_aborts_the_gate() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer(".abort".into());
    let (_id, dec, _) = arx.try_recv().unwrap();
    assert!(matches!(dec, Approval::Abort));
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

/// `.allow <prefix>` at the gate proposes a Command grant for that prefix; the
/// mechanism (not shown here) re-classifies the call against it (docs/permissions.md).
#[test]
fn allow_with_a_prefix_proposes_a_command_grant() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer(".allow git log".into());
    match arx.try_recv().unwrap().1 {
        Approval::ConfigEdit { change, .. } => assert_eq!(
            change,
            crate::log::ConfigChange::Command {
                command: "git log".into()
            }
        ),
        other => panic!("expected a config edit, got {other:?}"),
    }
}

/// Bare `.allow` on a `bash` gate derives the grant from the command's program.
#[test]
fn bare_allow_grants_the_bash_program() {
    let (mut s, mut arx) = state_with_gate(); // gate is `ls`
    s.submit_composer(".allow".into());
    match arx.try_recv().unwrap().1 {
        Approval::ConfigEdit { change, .. } => assert_eq!(
            change,
            crate::log::ConfigChange::Command {
                command: "ls".into()
            }
        ),
        other => panic!("expected a config edit, got {other:?}"),
    }
}

#[test]
fn process_output_smooths_the_matching_process_tail() {
    let (mut s, _arx) = state_with_gate();
    s.processes = vec![BgProc {
        id: 1,
        command: "cargo watch".into(),
        call_id: None,
        spawned_at: 0,
        last_chunk_at: None,
        tail: "tick 1\n".into(),
        running_secs: 3,
        idle_secs: 0,
    }];
    // A delta for the live process appends to its tail.
    s.handle_event(TuiEvent::ProcessOutput {
        id: 1,
        chunk: "tick 2\n".into(),
    });
    assert_eq!(s.processes[0].tail, "tick 1\ntick 2\n");
    assert!(s.dirty, "panel repaints");
    // A delta for an unknown id is ignored (it appears with the next state change).
    s.handle_event(TuiEvent::ProcessOutput {
        id: 9,
        chunk: "nope".into(),
    });
    assert_eq!(s.processes[0].tail, "tick 1\ntick 2\n");
}

#[test]
fn process_output_bounds_the_tail_on_a_char_boundary() {
    let (mut s, _arx) = state_with_gate();
    s.processes = vec![BgProc {
        id: 1,
        command: "chatty".into(),
        call_id: None,
        spawned_at: 0,
        last_chunk_at: None,
        tail: String::new(),
        running_secs: 1,
        idle_secs: 0,
    }];
    // A multibyte burst past the cap must not panic and must stay valid UTF-8.
    let chunk = "é".repeat(BG_TAIL_CAP); // 2 bytes each → 2× the cap
    s.handle_event(TuiEvent::ProcessOutput { id: 1, chunk });
    assert!(s.processes[0].tail.len() <= BG_TAIL_CAP);
    assert!(
        s.processes[0].tail.chars().all(|c| c == 'é'),
        "no codepoint sliced"
    );
}

#[test]
fn submit_without_gate_is_a_normal_turn() {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::tui::senders::Senders::channels();
    let (mut srx, mut arx) = (receivers.submit, receivers.approve);
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.submit_composer("hello".into());
    assert_eq!(
        srx.try_recv().unwrap().text,
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
    let (senders, receivers) = crate::tui::senders::Senders::channels();
    let (mut srx, mut atx) = (receivers.submit, receivers.attachments);
    let mut s = InteractiveState::new(erx, senders, rc, theme);

    let short = "one\ntwo\nthree";
    s.handle_paste(short.into());
    assert_eq!(s.composer.text(), short, "a short paste inlines");

    let long = (0..40)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    s.handle_paste(long.clone());
    assert_eq!(s.composer.text(), short, "the composer is untouched");
    let crate::tui::staged::AttachCmd::Upload { key, name, bytes } = atx.try_recv().unwrap() else {
        panic!("a long paste uploads")
    };
    assert!(
        name.starts_with("paste-") && name.ends_with(".txt"),
        "{name}"
    );
    assert_eq!(bytes, long.as_bytes());

    s.submit_composer("look".into());
    assert!(srx.try_recv().is_err(), "a send waits for its upload");

    let arrival = crate::attachments::Arrival {
        name: name.clone(),
        path: format!("att/composer/{name}"),
        kind: crate::attachments::Kind::Binary { bytes: 1 },
    };
    s.upload_landed(key, Ok(arrival.clone()));
    s.submit_composer("look".into());
    let sent = srx.try_recv().unwrap();
    assert_eq!(sent.attachments, vec![arrival]);
    assert!(s.staged.is_empty(), "sent chips leave the row");
}

/// A `409 cold` hands the refused turn back to the composer, and sending again
/// carries `accept_cold_read` — once (backlog: cold-session.md).
#[test]
fn a_cold_refusal_returns_the_turn_and_arms_consent() {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::tui::senders::Senders::channels();
    let mut srx = receivers.submit;
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.submit_composer("hello".into());
    let refused = srx.try_recv().unwrap();
    assert!(!refused.accept_cold_read);

    s.apply_cold(
        Some(refused),
        crate::client::ColdRefusal {
            message: "cold".into(),
            cold: serde_json::json!({ "since": 1, "context_tokens": 120_000 }),
        },
    );
    assert!(s.pending_turns.is_empty(), "the echo is withdrawn");
    assert_eq!(s.composer.text(), "hello");

    s.submit_composer("hello".into());
    assert!(srx.try_recv().unwrap().accept_cold_read);
    s.submit_composer("again".into());
    assert!(
        !srx.try_recv().unwrap().accept_cold_read,
        "consent is one send"
    );
}

/// `C-o` asks for a fold, and a pending consent is dropped with it: folding is the
/// other answer to the same cold refusal (backlog: cold-session.md).
#[test]
fn ctrl_o_folds_and_drops_a_pending_consent() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (senders, receivers) = crate::tui::senders::Senders::channels();
    let mut frx = receivers.fold;
    let mut s = InteractiveState::new(erx, senders, rc, theme);
    s.cold_consent = true;
    s.handle_composer_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(frx.try_recv().is_ok(), "a fold was asked for");
    assert!(!s.cold_consent);
}

/// The operation text shows only while the loop is running, follows the topic, and
/// clears with it (docs/tui.md): an idle session's keepalive is not a wait.
#[test]
fn the_operation_text_follows_the_topic_while_running() {
    let (mut s, _arx) = state_with_gate();
    let now = crate::log::Timestamp::now().0;
    let topic = serde_json::json!({
        "requests": [{"request": 0, "origin": "turn", "stage": "sent", "since": now}],
        "tools": [],
    });
    let view = crate::tui::operations::parse_current_operations(&topic);

    assert!(!s.handle_event(TuiEvent::Operations(view)));
    assert_eq!(s.operation_text, None, "not shown while the loop is idle");

    assert!(!s.handle_event(TuiEvent::Running(true)));
    assert_eq!(
        s.operation_text.as_deref(),
        Some("waiting on the model · 0s")
    );

    assert!(!s.handle_event(TuiEvent::Operations(None)));
    assert_eq!(s.operation_text, None);
}

/// A pending gate (or `ask`) owns the status bar's operation slot: a side request
/// open beside it is not what the operator is being waited on for (docs/tui.md).
#[test]
fn a_pending_gate_or_ask_leads_the_operation_text() {
    let (mut s, _arx) = state_with_gate();
    let now = crate::log::Timestamp::now().0;
    let topic = serde_json::json!({
        "requests": [{"request": 0, "origin": "title", "stage": "sent", "since": now}],
        "tools": [],
    });
    s.handle_event(TuiEvent::Running(true));
    s.handle_event(TuiEvent::Operations(
        crate::tui::operations::parse_current_operations(&topic),
    ));
    assert!(s.pending_gate.is_some());
    assert_eq!(
        s.status_operation_text().as_deref(),
        Some("waiting for your approval")
    );

    s.pending_gate = None;
    assert_eq!(
        s.status_operation_text().as_deref(),
        Some("writing a title · 0s")
    );
}
