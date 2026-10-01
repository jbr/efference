//! The environment a launch establishes, and how a hook ends.
//!
//! The hooks are `/bin/sh` scripts written as explicit argv arrays. That is the
//! shape the feature asks an operator to configure — a shell *in* the argv,
//! never one the harness supplies — so the tests exercise the real path rather
//! than a stand-in, and a `sh -c` recipe is one a user would actually write.
//!
//! The lifecycle tests are written against the orderings that matter: EOF
//! without an exit, an exit without EOF, a descendant outliving the hook, and a
//! future dropped rather than awaited. Each asserts the *outcome*, because the
//! earlier lifecycle reported success for several of these.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

/// A scratch directory that removes itself. Tests need a real absolute cwd (the
/// hook is run in one) and somewhere for a script to leave a marker.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("efference-launch-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// An argv running this `/bin/sh` script.
    fn script(&self, name: &str, body: &str) -> Vec<String> {
        let path = self.0.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
        vec!["/bin/sh".to_string(), path.to_string_lossy().into_owned()]
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host environment to inherit from and to replace.
fn host_env(entries: &[(&str, &str)]) -> EnvSnapshot {
    let values: BTreeMap<String, String> = entries
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    EnvSnapshot::new(values).expect("valid environment")
}

/// The entries of a snapshot, for comparison against a [`host_env`].
fn entries(snapshot: &EnvSnapshot) -> BTreeMap<String, String> {
    snapshot
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// An argv whose stdout is exactly this `printf` format string — the shape
/// `env -0` produces, written out so a test can also write malformed output.
///
/// Double quotes, so a format may still interpolate (`PWD=$(pwd)`); the
/// backslashes reach printf either way and it decodes them. The delimiter is
/// `\000` because this `printf` treats a bare `\0` as two characters.
fn dumping_hook(format: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf \"{format}\""),
    ]
}

/// Run with the documented bounds.
async fn launch(
    cwd: &Path,
    hook: Option<&[String]>,
    host: &EnvSnapshot,
) -> Result<EnvSnapshot, LaunchError> {
    environment_at(cwd, hook, host, std::future::pending()).await
}

/// Run with bounds a test can wait for.
async fn launch_with(
    cwd: &Path,
    hook: Option<&[String]>,
    host: &EnvSnapshot,
    limits: HookLimits,
) -> Result<EnvSnapshot, LaunchError> {
    run_hook(cwd, hook, host, std::future::pending(), limits).await
}

/// Bounds short enough to keep a lifecycle test fast.
fn quick(timeout_ms: u64) -> HookLimits {
    HookLimits {
        timeout: Duration::from_millis(timeout_ms),
        max_output: HOOK_OUTPUT_LIMIT,
    }
}

#[tokio::test]
async fn without_a_hook_the_environment_is_the_hosts() {
    let cwd = Scratch::new("no-hook");
    let host = host_env(&[("PATH", "/usr/bin"), ("HOST_ONLY", "kept")]);

    let established = launch(cwd.path(), None, &host)
        .await
        .expect("no hook is not a failure");

    assert_eq!(entries(&established), entries(&host));
}

#[tokio::test]
async fn a_hook_that_reports_an_environment_replaces_the_host_one() {
    let cwd = Scratch::new("replaces");
    let host = host_env(&[("HOST_ONLY", "gone"), ("SHARED", "old")]);
    let hook = dumping_hook("SHARED=new\\000FROM_HOOK=yes\\000");

    let established = launch(cwd.path(), Some(&hook), &host)
        .await
        .expect("hook ran");

    assert_eq!(
        entries(&established),
        BTreeMap::from([
            ("FROM_HOOK".to_string(), "yes".to_string()),
            ("SHARED".to_string(), "new".to_string()),
        ]),
        "the hook's output is the whole environment: a host variable it did not \
         report is not merged back in",
    );
}

#[tokio::test]
async fn the_hook_is_invoked_as_written() {
    // A program that is not a shell at all — which is the point: nothing here is
    // quoted, split, or handed to a shell.
    let cwd = Scratch::new("argv");
    let hook = vec!["/usr/bin/env".to_string(), "-0".to_string()];

    let established = launch(
        cwd.path(),
        Some(&hook),
        &host_env(&[("MARKER", "seen"), ("PATH", "/usr/bin:/bin")]),
    )
    .await
    .expect("`env -0` is the documented recipe");

    let values = entries(&established);
    assert_eq!(values.get("MARKER").map(String::as_str), Some("seen"));
    assert!(
        values.contains_key("PATH"),
        "an `env -0` hook reports the host environment it inherited",
    );
}

#[tokio::test]
async fn arguments_after_the_program_reach_the_hook_intact() {
    let cwd = Scratch::new("args");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // `$0` is the first argument after the command string.
        "printf 'FIRST=%s\\000SECOND=%s\\000' \"$0\" \"$1\"".to_string(),
        "one".to_string(),
        "two".to_string(),
    ];

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("hook ran");

    assert_eq!(
        entries(&established),
        BTreeMap::from([
            ("FIRST".to_string(), "one".to_string()),
            ("SECOND".to_string(), "two".to_string()),
        ]),
    );
}

#[tokio::test]
async fn the_hook_runs_in_the_requested_directory() {
    let cwd = Scratch::new("cwd");
    let hook = dumping_hook("PWD=$(pwd)\\000");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("hook ran");

    let reported = entries(&established)
        .remove("PWD")
        .expect("the hook reported PWD");
    // macOS reports `/private/var` for `/var`, so compare what the OS resolves
    // the directory to rather than the spelling we passed.
    let expected = std::fs::canonicalize(cwd.path()).expect("canonical scratch path");
    let reported = std::fs::canonicalize(reported).expect("canonical reported path");
    assert_eq!(reported, expected);
}

#[tokio::test]
async fn the_hook_inherits_the_host_environment() {
    let cwd = Scratch::new("inherits");
    let host = host_env(&[("FROM_HOST", "inherited"), ("PATH", "/usr/bin:/bin")]);
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'SEEN=%s\\000' \"$FROM_HOST\"".to_string(),
    ];

    let established = launch(cwd.path(), Some(&hook), &host)
        .await
        .expect("hook ran");

    assert_eq!(
        entries(&established)
            .remove("SEEN")
            .expect("the hook read the host variable"),
        "inherited",
    );
}

#[tokio::test]
async fn the_hook_does_not_see_the_host_processs_own_environment() {
    // `env -0` reports whatever environment it was given. Handed only MARKER, the
    // result must be exactly MARKER: a process variable leaking through would
    // show up as an extra entry. Invoked directly, so no shell is involved that
    // could synthesize a default of its own, and nothing global is mutated —
    // these tests run concurrently.
    let cwd = Scratch::new("cleared");
    let hook = vec!["/usr/bin/env".to_string(), "-0".to_string()];

    let established = launch(cwd.path(), Some(&hook), &host_env(&[("MARKER", "only")]))
        .await
        .expect("hook ran");

    assert_eq!(
        entries(&established),
        BTreeMap::from([("MARKER".to_string(), "only".to_string())]),
        "the process environment was cleared, so only what was injected is reported",
    );
}

#[tokio::test]
async fn a_value_may_contain_newlines_and_equals_signs() {
    let cwd = Scratch::new("values");
    // One printf, so the newlines inside `MULTI` survive to be part of its value.
    let hook = dumping_hook("MULTI=a\\nb=c\\nd\\000SIGNED=x=y=z\\000EMPTY=\\000");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("hook ran");

    assert_eq!(
        entries(&established),
        BTreeMap::from([
            ("EMPTY".to_string(), String::new()),
            ("MULTI".to_string(), "a\nb=c\nd".to_string()),
            // Split at the *first* `=`, so the rest belongs to the value.
            ("SIGNED".to_string(), "x=y=z".to_string()),
        ]),
    );
}

#[tokio::test]
async fn the_empty_records_env_zero_ends_with_are_tolerated() {
    let cwd = Scratch::new("trailing");
    // What `env -0` writes: a trailing delimiter, and here a doubled one.
    let hook = dumping_hook("A=1\\000B=2\\000\\000");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect("hook ran");

    assert_eq!(entries(&established).len(), 2);
}

#[tokio::test]
async fn no_output_with_a_successful_exit_is_an_empty_environment() {
    let cwd = Scratch::new("empty");
    let hook = dumping_hook("");

    let established = launch(cwd.path(), Some(&hook), &host_env(&[("HOST", "gone")]))
        .await
        .expect("a deliberately cleared baseline is valid");

    assert!(
        entries(&established).is_empty(),
        "the hook reported no environment, so the session's base is empty",
    );
}

#[tokio::test]
async fn an_unterminated_final_record_is_rejected() {
    // A last record with no closing delimiter is what a truncated write looks
    // like; accepting it would seed a session with a partial environment.
    let cwd = Scratch::new("unterminated");
    let hook = dumping_hook("A=1\\000B=hunter2");

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a dump that does not end in a delimiter is refused");

    assert!(
        matches!(&error, LaunchError::Unterminated { .. }),
        "expected Unterminated, got {error:?}",
    );
    assert!(
        !error.to_string().contains("hunter2"),
        "the diagnostic must not quote the record: {error}",
    );
}

#[tokio::test]
async fn a_record_without_an_equals_sign_is_rejected() {
    let cwd = Scratch::new("no-equals");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // The second record names no variable: no `=` at all.
        "printf 'A=1\\000TOKENhunter2\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a record with no `=` names no variable");

    assert!(
        matches!(&error, LaunchError::RecordWithoutName { record: 2, .. }),
        "expected the second record rejected, got {error:?}",
    );
    let text = error.to_string();
    assert!(
        text.contains("record 2"),
        "the position is reported: {text}"
    );
    assert!(
        !text.contains("hunter2"),
        "a malformed-output diagnostic must not quote the record: {text}",
    );
}

#[tokio::test]
async fn a_byte_that_is_not_utf8_is_rejected() {
    let cwd = Scratch::new("not-utf8");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=\\377\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a non-UTF-8 value cannot become an environment entry");

    assert!(
        matches!(
            &error,
            LaunchError::RecordNotUtf8 {
                record: 1,
                part: "value",
                ..
            }
        ),
        "expected the value rejected, got {error:?}",
    );
}

#[tokio::test]
async fn a_name_that_is_not_utf8_is_rejected() {
    let cwd = Scratch::new("bad-name");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '\\377=1\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a non-UTF-8 name cannot become an environment entry");

    assert!(
        matches!(
            &error,
            LaunchError::RecordNotUtf8 {
                record: 1,
                part: "name",
                ..
            }
        ),
        "expected the name rejected, got {error:?}",
    );
}

#[tokio::test]
async fn an_empty_variable_name_is_rejected_by_the_existing_constraints() {
    let cwd = Scratch::new("empty-name");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '=novalue\\000'".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("an entry with no name is not an environment");

    assert!(
        matches!(&error, LaunchError::Environment { .. }),
        "the record splits cleanly, so this is the environment's own validation: \
         {error:?}",
    );
}

#[tokio::test]
async fn a_relative_directory_is_refused_rather_than_run_in_the_hosts_own() {
    let error = launch(Path::new("relative/path"), None, &host_env(&[]))
        .await
        .expect_err("a relative directory would resolve against the host's cwd");

    assert!(
        matches!(&error, LaunchError::RelativeCwd { .. }),
        "expected RelativeCwd, got {error:?}",
    );
}

#[tokio::test]
async fn a_nonzero_exit_fails_without_an_environment() {
    let cwd = Scratch::new("exit");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=1\\000'; exit 3".to_string(),
    ];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("a failing hook fails the launch");

    match &error {
        LaunchError::Exited { status, .. } => assert_eq!(status.code(), Some(3)),
        other => panic!("expected a nonzero exit, got {other:?}"),
    }
}

#[tokio::test]
async fn a_hook_that_closes_stdout_and_then_fails_is_not_reported_successful() {
    // EOF on stdout is not the process finishing. The earlier lifecycle took
    // stdout-at-EOF as success and answered Ok here, before the exit was known.
    let cwd = Scratch::new("eof-then-fail");
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
    let cwd = Scratch::new("eof-then-ok");
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
    let cwd = Scratch::new("closed-and-running");
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
    // so a reader waiting for EOF would wait for the stranger (src/spawn.rs
    // documents this stalling another session).
    let cwd = Scratch::new("holds-stdout");
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
    let cwd = Scratch::new("closes-pipes");
    let marker = cwd.marker("survived");
    let hook = cwd.script(
        "closer.sh",
        &format!(
            "/bin/sh -c 'exec 1>&- 2>&-; sleep 1; : > {}'",
            marker.display()
        ),
    );

    let error = launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(200))
        .await
        .expect_err("the hook outlasts its deadline");

    assert!(
        matches!(&error, LaunchError::TimedOut { .. }),
        "expected a timeout, got {error:?}",
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !marker.exists(),
        "a descendant that closed its pipes outlived the timeout, so the group \
         was not killed",
    );
}

#[tokio::test]
async fn a_timeout_reaches_the_hoks_whole_process_tree() {
    // A shell recipe spawns children. Killing only the process the harness spawned
    // would leave those running, so the kill is aimed at the process group — and
    // this is what tells the two apart: the marker is written by a *grandchild*,
    // which outlives its parent unless the group goes too.
    let cwd = Scratch::new("cleanup");
    let marker = cwd.marker("survived");
    let hook = cwd.script(
        "tree.sh",
        &format!("/bin/sh -c 'sleep 2; : > {}'", marker.display()),
    );

    launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(200))
        .await
        .expect_err("the hook overruns its limit");

    // Long enough for the grandchild to have written the marker had it survived.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !marker.exists(),
        "the grandchild outlived the timeout, so the kill did not reach the \
         process group",
    );
}

#[tokio::test]
async fn a_dropped_future_still_kills_the_group() {
    // Cancellation that never runs the code after the await: the future is polled
    // once and dropped, as a cancelled request would. Only a kill in `Drop`
    // covers this — the success and timeout paths never run.
    let cwd = Scratch::new("dropped");
    let marker = cwd.marker("survived");
    let hook = cwd.script(
        "dropped.sh",
        &format!("/bin/sh -c 'sleep 2; : > {}'", marker.display()),
    );

    let host = host_env(&[]);

    // Spawned as a task, so it is actually driven — an unpolled future never
    // spawns the hook at all. `abort` is what a cancelled request does: it drops
    // the future without the success or timeout paths ever running.
    let spawn_hook = || {
        let path = cwd.path().to_path_buf();
        let argv = hook.clone();
        let host = host.clone();
        tokio::spawn(async move {
            environment_at(&path, Some(&argv), &host, std::future::pending()).await
        })
    };

    // Control: a hook left to finish is not killed, so the descendant below really
    // does survive without one. Without this the second half could pass because
    // the marker never appears for some unrelated reason.
    let control = spawn_hook();
    let _ = tokio::time::timeout(Duration::from_secs(15), control).await;
    assert!(
        marker.exists(),
        "control: a hook that finishes on its own leaves its descendant alone",
    );
    std::fs::remove_file(&marker).expect("marker from the control run");

    // The same hook, aborted while it is still running.
    let dropped = spawn_hook();
    tokio::time::sleep(Duration::from_millis(300)).await;
    dropped.abort();
    let _ = dropped.await;

    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !marker.exists(),
        "the grandchild outlived an aborted launch, so the group was not killed",
    );
}

#[tokio::test]
async fn an_abort_cancels_the_launch() {
    let cwd = Scratch::new("abort");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ];

    let started = Instant::now();
    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]), async {
        tokio::time::sleep(Duration::from_millis(150)).await;
    })
    .await
    .expect_err("an abort ends the launch");

    assert!(
        matches!(&error, LaunchError::Aborted { .. }),
        "expected Aborted, got {error:?}",
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the abort was not waited out: {:?}",
        started.elapsed(),
    );
}

#[tokio::test]
async fn a_hook_that_writes_to_stderr_still_succeeds() {
    // stderr is discarded rather than piped, so there is no reader to keep up and
    // no pipe for a chatty startup script to fill. A hook that writes far more
    // than any pipe would hold must still succeed.
    let cwd = Scratch::new("noisy-stderr");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "i=0; while [ $i -lt 20000 ]; do echo noise >&2; i=$((i+1)); done; printf 'A=1\\000'"
            .to_string(),
    ];

    let established = launch_with(cwd.path(), Some(&hook), &host_env(&[]), quick(20_000))
        .await
        .expect("a noisy stderr does not decide the hook's success");

    assert_eq!(
        entries(&established).get("A").map(String::as_str),
        Some("1")
    );
}

#[tokio::test]
async fn output_over_the_limit_fails_and_the_hook_is_killed() {
    let cwd = Scratch::new("too-large");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=%01000d\\000' 1".to_string(),
    ];
    let limits = HookLimits {
        timeout: Duration::from_secs(10),
        max_output: 16,
    };

    let error = launch_with(cwd.path(), Some(&hook), &host_env(&[]), limits)
        .await
        .expect_err("output past the limit fails the launch");

    match &error {
        LaunchError::TooLarge { limit, .. } => assert_eq!(*limit, 16),
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[tokio::test]
async fn output_at_the_limit_is_accepted() {
    // The reader compares the whole chunk against the cap, so a record whose
    // output is exactly the cap must not be mistaken for one that passed it.
    let cwd = Scratch::new("at-limit");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=12345\\000'".to_string(),
    ];
    let limits = HookLimits {
        timeout: Duration::from_secs(10),
        // Seven characters, one delimiter byte.
        max_output: 8,
    };

    let established = launch_with(cwd.path(), Some(&hook), &host_env(&[]), limits)
        .await
        .expect("output exactly at the cap is within it");

    assert_eq!(
        entries(&established).get("A").map(String::as_str),
        Some("12345")
    );
}

#[tokio::test]
async fn a_program_that_cannot_be_run_is_an_error() {
    let cwd = Scratch::new("no-such-program");
    let hook = vec!["efference-no-such-hook-program".to_string()];

    let error = launch(cwd.path(), Some(&hook), &host_env(&[]))
        .await
        .expect_err("an unrunnable hook fails the launch");

    assert!(
        matches!(&error, LaunchError::Spawn { .. }),
        "expected a spawn failure, got {error:?}",
    );
}

#[tokio::test]
async fn an_empty_argv_is_rejected() {
    let cwd = Scratch::new("empty-argv");

    let error = run_hook(
        cwd.path(),
        Some(&[]),
        &host_env(&[]),
        std::future::pending(),
        HookLimits::default(),
    )
    .await
    .expect_err("an argv with no program cannot be run");

    assert!(
        matches!(&error, LaunchError::NoProgram),
        "expected NoProgram, got {error:?}",
    );
}

#[test]
fn the_documented_limits_are_the_ones_applied() {
    assert_eq!(HOOK_TIMEOUT, Duration::from_secs(30));
    assert_eq!(HOOK_OUTPUT_LIMIT, 1024 * 1024);
    let defaults = HookLimits::default();
    assert_eq!(defaults.timeout, HOOK_TIMEOUT);
    assert_eq!(defaults.max_output, HOOK_OUTPUT_LIMIT);
}
