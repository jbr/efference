
use super::*;
use crate::tui::render_context::RenderContext;
use tokio::sync::mpsc::UnboundedReceiver;

/// A state with one pending gate, plus the receiver the decision lands on.
fn state_with_gate() -> (
    InteractiveState<'static>,
    UnboundedReceiver<(String, Approval)>,
) {
    let rc = RenderContext::new();
    let theme = InteractiveTheme::from_render_context(&rc);
    let (_etx, erx) = crossbeam_channel::unbounded();
    let (stx, _srx) = tokio::sync::mpsc::unbounded_channel();
    let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let (itx, _irx) = tokio::sync::mpsc::unbounded_channel();
    let mut s = InteractiveState::new(erx, stx, atx, ptx, itx, rc, theme);
    s.pending_gate = Some(PendingGate {
        call_id: "toolu_1".into(),
        name: "bash".into(),
        input: serde_json::json!({ "command": "ls" }),
        reason: None,
    });
    (s, arx)
}

#[test]
fn empty_submit_approves_the_gate() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer(String::new());
    let (id, dec) = arx.try_recv().expect("a decision was sent");
    assert_eq!(id, "toolu_1");
    assert!(matches!(dec, Approval::Approve));
    assert!(s.pending_gate.is_none(), "gate cleared after resolving");
}

#[test]
fn abort_sentinel_aborts_the_gate() {
    let (mut s, mut arx) = state_with_gate();
    s.submit_composer(".abort".into());
    let (_id, dec) = arx.try_recv().unwrap();
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
fn bg_output_smooths_the_matching_process_tail() {
    let (mut s, _arx) = state_with_gate();
    s.processes = vec![BgProc {
        handle: "bg#1".into(),
        command: "cargo watch".into(),
        tail: "tick 1\n".into(),
        running_secs: 3,
        idle_secs: 0,
    }];
    // A delta for the live process appends to its tail (between polls).
    s.handle_event(TuiEvent::BgOutput {
        handle: "bg#1".into(),
        chunk: "tick 2\n".into(),
    });
    assert_eq!(s.processes[0].tail, "tick 1\ntick 2\n");
    assert!(s.dirty, "panel repaints");
    // A delta for an unknown handle is ignored (it appears within one poll).
    s.handle_event(TuiEvent::BgOutput {
        handle: "bg#9".into(),
        chunk: "nope".into(),
    });
    assert_eq!(s.processes[0].tail, "tick 1\ntick 2\n");
}

#[test]
fn bg_output_bounds_the_tail_on_a_char_boundary() {
    let (mut s, _arx) = state_with_gate();
    s.processes = vec![BgProc {
        handle: "bg#1".into(),
        command: "chatty".into(),
        tail: String::new(),
        running_secs: 1,
        idle_secs: 0,
    }];
    // A multibyte burst past the cap must not panic and must stay valid UTF-8.
    let chunk = "é".repeat(BG_TAIL_CAP); // 2 bytes each → 2× the cap
    s.handle_event(TuiEvent::BgOutput {
        handle: "bg#1".into(),
        chunk,
    });
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
    let (stx, mut srx) = tokio::sync::mpsc::unbounded_channel();
    let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let (itx, _irx) = tokio::sync::mpsc::unbounded_channel();
    let mut s = InteractiveState::new(erx, stx, atx, ptx, itx, rc, theme);
    s.submit_composer("hello".into());
    assert_eq!(srx.try_recv().unwrap(), "hello", "went to the turn channel");
    assert!(arx.try_recv().is_err(), "not to the approval channel");
}
