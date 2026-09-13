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
    let (rtx, _rrx) = tokio::sync::mpsc::unbounded_channel();
    let (ctx, _crx) = tokio::sync::mpsc::unbounded_channel();
    let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let (itx, _irx) = tokio::sync::mpsc::unbounded_channel();
    let (btx, _brx) = tokio::sync::mpsc::unbounded_channel();
    let (imgtx, _imgrx) = tokio::sync::mpsc::unbounded_channel();
    let mut s = InteractiveState::new(erx, stx, rtx, ctx, atx, ptx, itx, btx, imgtx, rc, theme);
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
    let (stx, mut srx) = tokio::sync::mpsc::unbounded_channel();
    let (rtx, _rrx) = tokio::sync::mpsc::unbounded_channel();
    let (ctx, _crx) = tokio::sync::mpsc::unbounded_channel();
    let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let (itx, _irx) = tokio::sync::mpsc::unbounded_channel();
    let (btx, _brx) = tokio::sync::mpsc::unbounded_channel();
    let (imgtx, _imgrx) = tokio::sync::mpsc::unbounded_channel();
    let mut s = InteractiveState::new(erx, stx, rtx, ctx, atx, ptx, itx, btx, imgtx, rc, theme);
    s.submit_composer("hello".into());
    assert_eq!(
        srx.try_recv().unwrap().text,
        "hello",
        "went to the turn channel"
    );
    assert!(arx.try_recv().is_err(), "not to the approval channel");
}

/// The paste threshold (docs/inbox.md): a short paste goes into the composer, a long one goes
/// to the attachment store instead — the composer is left untouched, and the daemon's watcher
/// (not the TUI) is what announces the arrival.
#[test]
fn a_long_paste_lands_in_the_attachment_store() {
    let dir = std::env::temp_dir().join(format!("efference-tui-paste-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut s, _arx) = state_with_gate();
    s.attachments_dir = Some(dir.clone());

    let short = "one\ntwo\nthree";
    s.paste_text_or_attach(short);
    assert_eq!(s.composer.text(), short, "a short paste inlines");

    let long = (0..40)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    s.paste_text_or_attach(&long);
    assert_eq!(s.composer.text(), short, "the composer is untouched");
    let stored: Vec<_> = std::fs::read_dir(&dir)
        .expect("the store was created")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(stored.len(), 1);
    assert_eq!(std::fs::read_to_string(stored[0].path()).unwrap(), long);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// With no store — a gallery run or a read-only view — everything inlines, which is the
/// behaviour that existed before the store did.
#[test]
fn without_a_store_every_paste_inlines() {
    let (mut s, _arx) = state_with_gate();
    let long = (0..40).map(|n| format!("line {n}\n")).collect::<String>();
    s.paste_text_or_attach(&long);
    assert_eq!(s.composer.text(), long);
}
