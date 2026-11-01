//! Tests for `background.rs`.

use super::*;

/// Wait for the notification a background process posts when it exits, on its own slot
/// (docs/process-monitor.md).
async fn exit_notice(shared: &Session, handle: &str) {
    shared
        .wait_until("the exit notification", || {
            shared
                .pending_notifications()
                .iter()
                .any(|n| n.source == handle)
        })
        .await;
}

/// A backgrounded command runs end-to-end through the process monitor (docs/process-monitor.md): it
/// spawns, its output drains into the registry, and on exit it fires an `Ambient`
/// exit notification into the inbox — status + final tail — where a triage
/// would adjudicate it. Exercises the real spawn/drain/finish/notify path with no
/// model in the loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_process_drains_and_notifies_on_exit() {
    let (shared, _turns_rx) = Session::in_memory();
    let handle = shared
        .spawn_background("echo hello; echo world 1>&2".into(), None)
        .expect("spawn");
    assert_eq!(handle, "p#1");

    // The detached drain task runs on the runtime; wait for the exit notification to
    // land (the command exits near-immediately).
    exit_notice(&shared, "p#1").await;
    let pending = shared.pending_notifications();
    let n = pending.iter().find(|n| n.source == "p#1").unwrap();
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
    let (shared, _turns_rx) = Session::in_memory();
    let fg_id = Mutex::new(None::<u64>);
    // Detached once the first line is in the capture rather than after a fixed wait, which
    // bets on how soon bash starts and answers. The command has most of a second left. A
    // chunk landing in the capture signals nothing, so this wait polls.
    let detach = async {
        shared
            .poll_until("the first line is in the capture", || {
                let id = *fg_id.lock().unwrap();
                id.and_then(|id| shared.process_output_read(id, None))
                    .is_some_and(|read| read.content.contains("early"))
            })
            .await;
        Interrupt::Detach
    };
    let outcome = crate::tools::bash::Bash::default()
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
        crate::tools::bash::Outcome::Detached(d) => d,
        crate::tools::bash::Outcome::Done(r) => panic!("expected a detach, got {:?}", r.content),
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
    exit_notice(&shared, "p#1").await;
    let pending = shared.pending_notifications();
    let n = pending.iter().find(|n| n.source == "p#1").unwrap();
    assert!(n.body.contains("exited 0"), "clean exit: {}", n.body);
    assert!(
        n.body.contains("early") && n.body.contains("late"),
        "final tail spans the whole run: {}",
        n.body
    );
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
    // A chunk landing in the capture signals nothing, so this wait polls.
    shared
        .poll_until("$TMPDIR in the capture", || {
            shared
                .bg_read_since(&handle, None)
                .is_some_and(|read| read.contains("/scratch/efference-test"))
        })
        .await;
}

/// The detach note (docs/process-monitor.md): stashed by `PATCH /processes/:id {background: true}`,
/// consumed exactly once by the loop when a handoff lands; a blank note is
/// dropped at the door.
#[test]
fn detach_note_is_stashed_and_consumed_once() {
    let (shared, _turns_rx) = Session::in_memory();
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
    let (shared, _turns_rx) = Session::in_memory();
    assert!(!shared.kill_bg("p#1"), "unknown handle → false");

    let h = shared
        .spawn_background("sleep 30".into(), None)
        .expect("spawn");

    // Inbox read: empty while the process runs. Read before the kill, since the reaper
    // posts the process's exit to the inbox once the process is gone.
    assert!(
        shared.inbox_listing().contains("empty"),
        "{}",
        shared.inbox_listing()
    );
    assert!(shared.kill_bg(&h), "kills a running process");

    // The exit notice the reaper posts, then a notification beside it.
    exit_notice(&shared, &h).await;
    shared.notify(Notification::new("ci", "build failed", None));
    let summary = shared.inbox_listing();
    assert!(
        summary.contains(&h) && summary.contains("ci") && summary.contains("build failed"),
        "{summary}"
    );

    shared.kill_all_bg();
}
