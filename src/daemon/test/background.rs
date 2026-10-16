//! Tests for `background.rs`.

use super::*;

/// A backgrounded command runs end-to-end through the process monitor (docs/process-monitor.md): it
/// spawns, its output drains into the registry, and on exit it fires an `Ambient`
/// exit notification into the inbox — status + final tail — where a triage
/// would adjudicate it. Exercises the real spawn/drain/finish/notify path with no
/// model in the loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_process_drains_and_notifies_on_exit() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let handle = shared
        .spawn_background("echo hello; echo world 1>&2".into(), None)
        .expect("spawn");
    assert_eq!(handle, "p#1");

    // The detached drain task runs on the runtime; poll until the exit
    // notification lands (the command exits near-immediately).
    let mut tries = 0;
    loop {
        let pending = shared.pending_notifications();
        if let Some(n) = pending.iter().find(|n| n.source == "p#1") {
            assert!(
                n.body.contains("exited 0"),
                "exit status in body: {}",
                n.body
            );
            assert!(
                n.body.contains("hello") || n.body.contains("world"),
                "final tail carried: {}",
                n.body
            );
            assert_eq!(n.slot.as_deref(), Some("p#1"), "exit rides the bg slot");
            break;
        }
        tries += 1;
        assert!(tries < 100, "exit notification never arrived: {pending:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Exited → no longer in the ambient show or the teardown set.
    assert!(
        shared.bg_snapshot().is_empty(),
        "exited process off the triage"
    );
    shared.kill_all_bg(); // no-op (nothing running) — must not panic.
}

/// The foreground→background handoff end to end (docs/process-monitor.md): a foreground
/// bash command is registered at spawn and its chunks fed to the capture (the loop's
/// wiring, mirrored here), it is detached mid-run and flipped to background under
/// the same id, the continuing drain lands the post-detach output in the same
/// capture, and the exit notifies on the `p#` slot like any spawned background
/// process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_background_continues_the_drain_and_notifies_on_exit() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let detach = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Interrupt::Detach
    };
    let fg_id = std::sync::Mutex::new(None::<u64>);
    let outcome = crate::bash::Bash::default()
        .run(
            &serde_json::json!({ "command": "echo early; sleep 0.6; echo late" }),
            detach,
            |pgid| {
                *fg_id.lock().unwrap() = Some(shared.register_foreground(
                    "echo early; sleep 0.6; echo late".into(),
                    pgid,
                    None,
                ));
            },
            |chunk| {
                if let Some(id) = *fg_id.lock().unwrap() {
                    shared.push_process_chunk(id, chunk);
                }
            },
        )
        .await;
    let detached = match outcome {
        crate::bash::Outcome::Detached(d) => d,
        crate::bash::Outcome::Done(r) => panic!("expected a detach, got {:?}", r.content),
    };
    let id = fg_id.lock().unwrap().take().expect("registered at spawn");
    let handle = shared.adopt_background(*detached, id);
    assert_eq!(handle, "p#1");

    // The live-fed capture is readable immediately, before any new output.
    let read = shared.bg_read_since(&handle, None).expect("known handle");
    assert!(
        read.contains("early"),
        "pre-detach bytes in the capture: {read}"
    );

    // The continuing drain appends the post-detach output to the same
    // capture, and the exit notifies — one uninterrupted stream.
    let mut tries = 0;
    loop {
        let pending = shared.pending_notifications();
        if let Some(n) = pending.iter().find(|n| n.source == "p#1") {
            assert!(n.body.contains("exited 0"), "clean exit: {}", n.body);
            assert!(
                n.body.contains("early") && n.body.contains("late"),
                "final tail spans the whole run: {}",
                n.body
            );
            break;
        }
        tries += 1;
        assert!(tries < 100, "exit notification never arrived: {pending:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A background spawn exports the session scratch dir as `$TMPDIR`, mirroring
/// the foreground bash spawn (docs/tools.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_spawn_exports_the_scratch_tmpdir() {
    let (shared, _turns_rx) = Session::with_role_and_scratch(
        std::env::current_dir().unwrap(),
        Log::in_memory(),
        LoadedConfig::default(),
        Some(PathBuf::from("/scratch/efference-test")),
    );
    let handle = shared
        .spawn_background("echo $TMPDIR".into(), None)
        .expect("spawn");
    let mut tries = 0;
    loop {
        let read = shared.bg_read_since(&handle, None).expect("known handle");
        if read.contains("/scratch/efference-test") {
            break;
        }
        tries += 1;
        assert!(tries < 100, "TMPDIR never appeared in the capture: {read}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The detach note (docs/process-monitor.md): stashed by `PATCH /processes/:id {background: true}`,
/// consumed exactly once by the loop when a handoff lands; a blank note is
/// dropped at the door.
#[test]
fn detach_note_is_stashed_and_consumed_once() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.request_detach(Some("check the logs".into()));
    assert_eq!(shared.take_detach_note().as_deref(), Some("check the logs"));
    assert_eq!(shared.take_detach_note(), None, "consumed once");
    shared.request_detach(Some("   ".into()));
    assert_eq!(shared.take_detach_note(), None, "blank note dropped");
}

/// The directed Slice-2 reads/acts (docs/process-monitor.md, docs/inbox.md): `kill_bg` finds a running child
/// (and rejects unknown handles), and `inbox_listing` lists pending notifications.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_bg_and_inbox_summary() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert!(!shared.kill_bg("p#1"), "unknown handle → false");

    let h = shared
        .spawn_background("sleep 30".into(), None)
        .expect("spawn");
    assert!(shared.kill_bg(&h), "kills a running process");

    // Inbox read: empty, then surfaces a posted notification.
    assert!(
        shared.inbox_listing().contains("empty"),
        "{}",
        shared.inbox_listing()
    );
    shared.notify("ci".into(), "build failed".into(), None, None);
    let summary = shared.inbox_listing();
    assert!(
        summary.contains("ci") && summary.contains("build failed"),
        "{summary}"
    );

    shared.kill_all_bg();
}
