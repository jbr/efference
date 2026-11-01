//! How a hook ends: EOF against exit, the deadline, and cancellation.
//!
//! Each of these had a version that passed for the wrong reason, so the
//! synchronization is explicit rather than timed. A descendant announces itself
//! with a readiness marker, and the marker a survivor would write only appears if
//! the gate is opened — so "the marker is absent" means the process was killed,
//! not that the test outran it.

use std::time::{Duration, Instant};

use super::*;

#[tokio::test]
async fn a_hook_that_closes_stdout_and_then_fails_is_not_reported_successful() {
    // EOF on stdout is not the process finishing. The earlier lifecycle took
    // stdout-at-EOF as success and answered Ok here, before the exit was known.
    let cwd = Scratch::new("life-eof-then-fail");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=1\\000'; exec 1>&-; sleep 0.2; exit 3".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a hook that exits nonzero is a failure however its stdout ended");

    match &error {
        LaunchError::Exited { status, .. } => assert_eq!(status.code(), Some(3)),
        other => panic!("expected the nonzero exit to be observed, got {other:?}"),
    }
}

#[tokio::test]
async fn a_hook_that_closes_stdout_early_is_not_killed_for_it() {
    // The other half of the case above: closing stdout early is legal, so a hook
    // that does it and then exits cleanly must still be reported successful.
    let cwd = Scratch::new("life-eof-then-ok");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=1\\000'; exec 1>&-; exit 0".to_string(),
    ];

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("closing stdout is not a failure");

    assert_eq!(
        entries(&established).get("A").map(String::as_str),
        Some("1")
    );
}

#[tokio::test]
async fn a_hook_that_closes_both_pipes_and_keeps_running_is_bounded_by_the_deadline() {
    // Neither pipe carries anything more, so the read finishes at once and only
    // the exit status is outstanding. Waiting for that outside the deadline is
    // what left this unbounded.
    let cwd = Scratch::new("life-closed-and-running");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=1\\000'; exec 1>&- 2>&-; sleep 30".to_string(),
    ];

    let started = Instant::now();
    let error = launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(200))
        .await
        .expect_err("a hook that never exits is bounded by the deadline");

    assert!(
        matches!(&error, LaunchError::TimedOut { .. }),
        "expected a timeout, got {error:?}",
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the deadline covered the wait, not just the read: {:?}",
        started.elapsed(),
    );
}

#[tokio::test]
async fn a_descendant_holding_stdout_does_not_outlive_the_deadline() {
    // The parent exits at once; a background descendant keeps the write end open,
    // so a reader waiting for EOF would wait for the stranger (src/platform/spawn.rs
    // documents this stalling another session).
    let cwd = Scratch::new("life-holds-stdout");
    let hook = cwd.script("holder.sh", "(sleep 30) & printf 'A=1\\000'");

    let started = Instant::now();
    let error = launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(300))
        .await
        .expect_err("a descendant holding stdout is bounded by the deadline");

    assert!(
        matches!(&error, LaunchError::TimedOut { .. }),
        "expected a timeout, got {error:?}",
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "returned rather than waiting on the pipe: {:?}",
        started.elapsed(),
    );
}

#[tokio::test]
async fn a_descendant_that_closes_both_pipes_does_not_leave_the_hook_running() {
    // The group is killed on the terminating path whether or not the direct child
    // is still alive, so a descendant that has closed its pipes goes too.
    let cwd = Scratch::new("life-closes-pipes");
    let marker = cwd.marker("survived");
    let ready = cwd.marker("ready");
    let gate = cwd.marker("gate");
    // The parent closes both its pipes and keeps running, and so does the
    // descendant — so the read finishes at once and only the exit is outstanding.
    let hook = cwd.script(
        "closer.sh",
        &format!(
            "/bin/sh -c 'exec 1>&- 2>&-; : > {ready}; \
             while [ ! -f {gate} ]; do sleep 0.05; done; : > {marker}' &\n\
             exec 1>&- 2>&-\n\
             sleep 30\n",
            ready = ready.display(),
            gate = gate.display(),
            marker = marker.display(),
        ),
    );

    let error = launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(200))
        .await
        .expect_err("the hook outlasts its deadline");

    assert!(
        matches!(&error, LaunchError::TimedOut { .. }),
        "expected a timeout, got {error:?}",
    );
    await_marker(&ready, "the descendant announcing itself").await;
    std::fs::write(&gate, "").expect("open the gate");
    settle().await;
    assert!(
        !marker.exists(),
        "a descendant that closed its pipes outlived the timeout, so the group \
         was not killed",
    );
}

#[tokio::test]
async fn a_zero_exit_hook_with_unusable_output_still_kills_its_group() {
    // A zero exit is not success on its own: output that does not parse is a
    // failed launch, and the launch failed *after* the child exited, so the guard
    // is the only thing that can still take the descendants with it.
    let cwd = Scratch::new("life-parse-kills");
    let marker = cwd.marker("survived");
    let ready = cwd.marker("ready");
    let gate = cwd.marker("gate");
    let hook = exiting_hook(
        &cwd,
        "malformed.sh",
        "MALFORMED\\000",
        &ready,
        &gate,
        &marker,
    );

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("output with no `=` is not an environment");

    assert!(
        matches!(&error, LaunchError::RecordWithoutName { record: 1, .. }),
        "expected the record rejected, got {error:?}",
    );
    await_marker(&ready, "the descendant announcing itself").await;
    std::fs::write(&gate, "").expect("open the gate");
    settle().await;
    assert!(
        !marker.exists(),
        "the descendant of a hook whose output did not parse survived, so the \
         group was signalled too early",
    );
}

#[tokio::test]
async fn a_successful_hook_leaves_its_descendant_alone() {
    // The other half of the test above, so its negative assertion is meaningful:
    // a descendant the group did *not* kill does reach the marker.
    let cwd = Scratch::new("life-parse-spares");
    let marker = cwd.marker("survived");
    let ready = cwd.marker("ready");
    let gate = cwd.marker("gate");
    let hook = exiting_hook(&cwd, "valid.sh", "A=1\\000", &ready, &gate, &marker);

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("a parseable dump from a zero-exit hook is a success");

    assert_eq!(
        entries(&established).get("A").map(String::as_str),
        Some("1")
    );
    await_marker(&ready, "the descendant announcing itself").await;
    std::fs::write(&gate, "").expect("open the gate");
    settle().await;
    assert!(
        marker.exists(),
        "a successful launch must not kill its own group",
    );
}

#[tokio::test]
async fn a_timeout_reaches_the_hooks_whole_process_tree() {
    // A shell recipe spawns children. Killing only the process the harness spawned
    // would leave those running, so the kill is aimed at the process group — and
    // this is what tells the two apart: the marker is written by a *grandchild*,
    // which outlives its parent unless the group goes too.
    let cwd = Scratch::new("life-cleanup");
    let marker = cwd.marker("survived");
    let hook = cwd.script(
        "tree.sh",
        &format!("/bin/sh -c 'sleep 2; : > {}'", marker.display()),
    );

    launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(200))
        .await
        .expect_err("the hook overruns its limit");

    // Long enough for the grandchild to have written the marker had it survived.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        !marker.exists(),
        "the grandchild outlived the timeout, so the kill did not reach the \
         process group",
    );
}

#[tokio::test]
async fn a_dropped_future_still_kills_the_group() {
    // Spawned as a task, so it is actually driven — an unpolled future never
    // spawns the hook at all. `abort` is what a cancelled request does: it drops
    // the future without the success or timeout paths ever running.
    let cwd = Scratch::new("life-aborted");

    // Each run gets its own markers, so an absence in one cannot be explained by
    // the other having written it.
    let control_ready = cwd.marker("control-ready");
    let control_gate = cwd.marker("control-gate");
    let control_marker = cwd.marker("control-survived");
    let ready = cwd.marker("ready");
    let gate = cwd.marker("gate");
    let marker = cwd.marker("survived");

    // Control: an awaited launch that succeeds, so nothing signals its group.
    let mut control = spawn_hook(
        &cwd,
        &exiting_hook(
            &cwd,
            "control.sh",
            "A=1\\000",
            &control_ready,
            &control_gate,
            &control_marker,
        ),
    );
    if tokio::time::timeout(Duration::from_secs(20), &mut control)
        .await
        .is_err()
    {
        // Not a detached task left running: abort it and collect it before failing.
        control.abort();
        let _ = control.await;
        panic!("the control launch did not finish");
    }
    await_marker(&control_ready, "the control run's descendant").await;
    std::fs::write(&control_gate, "").expect("open the control gate");
    settle().await;
    assert!(
        control_marker.exists(),
        "a launch that finishes on its own leaves its descendant alive, or the \
         assertion below proves nothing",
    );

    // The cancelled run. Its hook waits for the gate, so it is still in flight —
    // and its guard still armed — when the abort lands.
    let cancelled = spawn_hook(
        &cwd,
        &waiting_hook(&cwd, "cancelled.sh", "A=1\\000", &ready, &gate, &marker),
    );
    // Wait for this invocation's own descendant to announce itself: aborting
    // before it started would prove nothing.
    await_marker(&ready, "the cancelled run's descendant").await;
    cancelled.abort();
    let _ = cancelled.await;

    std::fs::write(&gate, "").expect("open the gate");
    settle().await;
    assert!(
        !marker.exists(),
        "the descendant outlived an aborted launch, so the group was not killed",
    );
}

/// Spawn a launch as a task, for the cancellation tests: the borrowed signature
/// is not `'static`, so the values it borrows move into the spawned block.
fn spawn_hook(cwd: &Scratch, hook: &[String]) -> tokio::task::JoinHandle<()> {
    let path = cwd.path().to_path_buf();
    let argv = hook.to_vec();
    tokio::spawn(async move {
        let _ = environment_at(&path, Some(&argv), &host_env(&[]), std::future::pending()).await;
    })
}
