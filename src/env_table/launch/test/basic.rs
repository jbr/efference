//! What a launch establishes: the two sources, argv handling, the working
//! directory, inheritance, and the bounds.

use std::time::Duration;

use super::*;

#[tokio::test]
async fn without_a_hook_the_environment_is_the_hosts() {
    let cwd = Scratch::new("basic-no-hook");
    let host = host_env(&[("PATH", "/usr/bin"), ("HOST_ONLY", "kept")]);

    let established = launch(cwd.path(), None, &host)
        .await
        .expect("no hook is not a failure");

    assert_eq!(entries(&established), entries(&host));
}

#[tokio::test]
async fn a_hook_that_reports_an_environment_replaces_the_host_one() {
    let cwd = Scratch::new("basic-replaces");
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
    let cwd = Scratch::new("basic-argv");
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
    let cwd = Scratch::new("basic-args");
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
    let cwd = Scratch::new("basic-cwd");
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
    let cwd = Scratch::new("basic-inherits");
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
    let cwd = Scratch::new("basic-cleared");
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
    let cwd = Scratch::new("basic-exit");
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
async fn a_program_that_cannot_be_run_is_an_error() {
    let cwd = Scratch::new("basic-no-such-program");
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
    let cwd = Scratch::new("basic-empty-argv");

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

#[tokio::test]
async fn a_hook_that_writes_to_stderr_still_succeeds() {
    // stderr is discarded rather than piped, so there is no reader to keep up and
    // no pipe for a chatty startup script to fill. A hook that writes far more
    // than any pipe would hold must still succeed.
    let cwd = Scratch::new("basic-noisy-stderr");
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
    let cwd = Scratch::new("basic-too-large");
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
    let cwd = Scratch::new("basic-at-limit");
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

#[test]
fn the_documented_limits_are_the_ones_applied() {
    assert_eq!(HOOK_TIMEOUT, Duration::from_secs(30));
    assert_eq!(HOOK_OUTPUT_LIMIT, 1024 * 1024);
    let defaults = HookLimits::default();
    assert_eq!(defaults.timeout, HOOK_TIMEOUT);
    assert_eq!(defaults.max_output, HOOK_OUTPUT_LIMIT);
}
