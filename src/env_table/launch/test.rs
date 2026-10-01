//! The environment a launch establishes: argv handling, cwd, inheritance,
//! replacement, the record grammar, validation, and the bounds.
//!
//! The hooks here are `/bin/sh` scripts written as explicit argv arrays. That is
//! the shape the feature asks an operator to configure — a shell *in* the argv,
//! never one the harness supplies — so the tests exercise the real path rather
//! than a stand-in, and a `sh -c` recipe is one a user would actually write.

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

    /// Write an executable `/bin/sh` script and return it as a one-element argv.
    fn script(&self, name: &str, body: &str) -> Vec<String> {
        let path = self.0.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
        vec!["/bin/sh".to_string(), path.to_string_lossy().into_owned()]
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

/// A hook whose stdout is exactly this `printf` format string — the shape
/// `env -0` produces, written out so a test can also write malformed output.
///
/// The separator is `\\000` and not `\\0` because this `printf` treats a bare
/// `\\0` as two characters; octal escapes are what it decodes.
fn dumping_hook(format: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // Double quotes, so a format may still interpolate (`PWD=$(pwd)`); the
        // backslashes reach printf either way and it decodes them.
        format!("printf \"{format}\""),
    ]
}

#[test]
fn without_a_hook_the_environment_is_the_hosts() {
    let cwd = Scratch::new("no-hook");
    let host = host_env(&[("PATH", "/usr/bin"), ("HOST_ONLY", "kept")]);

    let established = environment_at(cwd.path(), None, &host).expect("no hook is not a failure");

    assert_eq!(entries(&established), entries(&host));
}

#[test]
fn a_hook_that_reports_an_environment_replaces_the_host_one() {
    let cwd = Scratch::new("replaces");
    let host = host_env(&[("HOST_ONLY", "gone"), ("SHARED", "old")]);
    let hook = dumping_hook("SHARED=new\\000FROM_HOOK=yes\\000");

    let established = environment_at(cwd.path(), Some(&hook), &host).expect("hook ran");

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

#[test]
fn the_hook_is_invoked_as_written() {
    // A program that is not a shell at all, with an argument that would be a
    // syntax error to anything interpreting it — which is the point: nothing here
    // is quoted, split, or handed to a shell.
    let cwd = Scratch::new("argv");
    let hook = vec!["/usr/bin/env".to_string(), "-0".to_string()];

    let established = environment_at(
        cwd.path(),
        Some(&hook),
        &host_env(&[("MARKER", "seen"), ("PATH", "/usr/bin:/bin")]),
    )
    .expect("`env -0` is the documented recipe");

    let values = entries(&established);
    assert_eq!(values.get("MARKER").map(String::as_str), Some("seen"));
    assert!(
        values.contains_key("PATH"),
        "an `env -0` hook reports the host environment it inherited",
    );
}

#[test]
fn arguments_after_the_program_reach_the_hook_intact() {
    let cwd = Scratch::new("args");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // `$0` is the first argument after the command string.
        "printf 'FIRST=%s\\0SECOND=%s\\0' \"$0\" \"$1\"".to_string(),
        "one".to_string(),
        "two".to_string(),
    ];

    let established = environment_at(cwd.path(), Some(&hook), &host_env(&[])).expect("hook ran");

    assert_eq!(
        entries(&established),
        BTreeMap::from([
            ("FIRST".to_string(), "one".to_string()),
            ("SECOND".to_string(), "two".to_string()),
        ]),
    );
}

#[test]
fn the_hook_runs_in_the_requested_directory() {
    let cwd = Scratch::new("cwd");
    let hook = dumping_hook("PWD=$(pwd)\\000");

    let established = environment_at(cwd.path(), Some(&hook), &host_env(&[])).expect("hook ran");

    let reported = entries(&established)
        .remove("PWD")
        .expect("the hook reported PWD");
    // macOS reports `/private/var` for `/var`, so compare what the OS resolves
    // the directory to rather than the spelling we passed.
    let expected = std::fs::canonicalize(cwd.path()).expect("canonical scratch path");
    let reported = std::fs::canonicalize(reported).expect("canonical reported path");
    assert_eq!(reported, expected);
}

#[test]
fn the_hook_inherits_the_host_environment() {
    let cwd = Scratch::new("inherits");
    let host = host_env(&[("FROM_HOST", "inherited"), ("PATH", "/usr/bin:/bin")]);
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'SEEN=%s\\000' \"$FROM_HOST\"".to_string(),
    ];

    let established = environment_at(cwd.path(), Some(&hook), &host).expect("hook ran");

    assert_eq!(
        entries(&established)
            .remove("SEEN")
            .expect("the hook read the host variable"),
        "inherited",
    );
}

#[test]
fn the_hook_does_not_see_the_host_processs_own_environment() {
    // The host environment is applied with `env_clear`, so the process running
    // the test contributes nothing — a variable present only here must not reach
    // the hook.
    let unique = format!("EFFERENCE_LAUNCH_TEST_{}", std::process::id());
    // SAFETY: a single-threaded test process, and this name is not otherwise set.
    unsafe { std::env::set_var(&unique, "leaked") };
    let cwd = Scratch::new("cleared");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf 'LEAK=%s\\000' \"${unique}\""),
    ];

    let established = environment_at(cwd.path(), Some(&hook), &host_env(&[])).expect("hook ran");
    // SAFETY: as above; nothing else reads this name.
    unsafe { std::env::remove_var(&unique) };

    assert_eq!(
        entries(&established).remove("LEAK").as_deref(),
        Some(""),
        "the process environment was cleared, so the hook saw an unset variable",
    );
}

#[test]
fn a_value_may_contain_newlines_and_equals_signs() {
    let cwd = Scratch::new("values");
    // One printf, so the newlines inside `MULTI` survive to be part of its value.
    let hook = dumping_hook("MULTI=a\\nb=c\\nd\\000SIGNED=x=y=z\\000EMPTY=\\000");

    let established = environment_at(cwd.path(), Some(&hook), &host_env(&[])).expect("hook ran");

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

#[test]
fn the_empty_records_env_zero_ends_with_are_tolerated() {
    let cwd = Scratch::new("trailing");
    // What `env -0` writes: a trailing delimiter, and here a doubled one.
    let hook = dumping_hook("A=1\\000B=2\\000\\000");

    let established = environment_at(cwd.path(), Some(&hook), &host_env(&[])).expect("hook ran");

    assert_eq!(entries(&established).len(), 2);
}

#[test]
fn a_record_without_an_equals_sign_is_rejected() {
    let cwd = Scratch::new("no-equals");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // The second record names no variable: no `=` at all.
        "printf 'A=1\\000TOKENhunter2'".to_string(),
    ];

    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]))
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

#[test]
fn a_byte_that_is_not_utf8_is_rejected() {
    let cwd = Scratch::new("not-utf8");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=\\377\\0'".to_string(),
    ];

    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]))
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

#[test]
fn a_name_that_is_not_utf8_is_rejected() {
    let cwd = Scratch::new("bad-name");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '\\377=1\\0'".to_string(),
    ];

    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]))
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

#[test]
fn an_empty_variable_name_is_rejected_by_the_existing_constraints() {
    let cwd = Scratch::new("empty-name");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '=novalue\\0'".to_string(),
    ];

    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]))
        .expect_err("an entry with no name is not an environment");

    assert!(
        matches!(&error, LaunchError::Environment { .. }),
        "the record splits cleanly, so this is the environment's own validation: \
         {error:?}",
    );
}

#[test]
fn a_nonzero_exit_fails_without_an_environment() {
    let cwd = Scratch::new("exit");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=1\\0'; exit 3".to_string(),
    ];

    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]))
        .expect_err("a failing hook fails the launch");

    match &error {
        LaunchError::Exited { status, .. } => assert_eq!(status.code(), Some(3)),
        other => panic!("expected a nonzero exit, got {other:?}"),
    }
    assert!(
        !error.to_string().contains("A=1"),
        "the output it did write is not reported: {error}",
    );
}

#[test]
fn a_hook_that_writes_to_stderr_does_not_leak_it_into_the_error() {
    let cwd = Scratch::new("stderr");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'TOKEN=hunter2\\n' >&2; exit 1".to_string(),
    ];

    let error =
        environment_at(cwd.path(), Some(&hook), &host_env(&[])).expect_err("the hook failed");

    assert!(
        !error.to_string().contains("hunter2"),
        "a startup script may print secrets, so stderr is drained and dropped: {error}",
    );
}

#[test]
fn a_program_that_cannot_be_run_is_an_error() {
    let cwd = Scratch::new("no-such-program");
    let hook = vec!["efference-no-such-hook-program".to_string()];

    let error = environment_at(cwd.path(), Some(&hook), &host_env(&[]))
        .expect_err("an unrunnable hook fails the launch");

    assert!(
        matches!(&error, LaunchError::Spawn { .. }),
        "expected a spawn failure, got {error:?}",
    );
}

#[test]
fn an_empty_argv_is_rejected() {
    let cwd = Scratch::new("empty-argv");

    let error = run_hook(cwd.path(), Some(&[]), &host_env(&[]), HookLimits::default())
        .expect_err("an argv with no program cannot be run");

    assert!(
        matches!(&error, LaunchError::NoProgram),
        "expected NoProgram, got {error:?}",
    );
}

#[test]
fn output_over_the_limit_fails_and_the_hook_is_killed() {
    let cwd = Scratch::new("too-large");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=%01000d\\0' 1".to_string(),
    ];
    let limits = HookLimits {
        timeout: Duration::from_secs(10),
        max_output: 16,
    };

    let error = run_hook(cwd.path(), Some(&hook), &host_env(&[]), limits)
        .expect_err("output past the limit fails the launch");

    match &error {
        LaunchError::TooLarge { limit, .. } => assert_eq!(*limit, 16),
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn output_at_the_limit_is_accepted() {
    // The reader reads one byte past the cap to tell "at" from "over"; a hook
    // whose output is exactly the cap must not be mistaken for one that passed it.
    let cwd = Scratch::new("at-limit");
    // Seven characters, one delimiter byte.
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf 'A=12345\\000'".to_string(),
    ];
    let limits = HookLimits {
        timeout: Duration::from_secs(10),
        max_output: 8,
    };

    let established = run_hook(cwd.path(), Some(&hook), &host_env(&[]), limits)
        .expect("output exactly at the cap is within it");

    assert_eq!(
        entries(&established).get("A").map(String::as_str),
        Some("12345"),
    );
}

#[test]
fn a_hook_that_never_finishes_is_killed() {
    let cwd = Scratch::new("timeout");
    let hook = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ];
    let limits = HookLimits {
        timeout: Duration::from_millis(200),
        max_output: HOOK_OUTPUT_LIMIT,
    };

    let started = Instant::now();
    let error = run_hook(cwd.path(), Some(&hook), &host_env(&[]), limits)
        .expect_err("a hook that overruns fails the launch");

    assert!(
        matches!(&error, LaunchError::TimedOut { limit, .. } if *limit == Duration::from_millis(200)),
        "expected a timeout carrying the limit, got {error:?}",
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the hook was killed rather than waited out: {:?}",
        started.elapsed(),
    );
}

#[test]
fn a_timeout_reaches_the_hoks_whole_process_tree() {
    // A shell recipe spawns children. Killing only the process the harness spawned
    // would leave those running, so the kill is aimed at the process group — and
    // this is what tells the two apart: the marker is written by a *grandchild*,
    // which outlives its parent unless the group goes too.
    let cwd = Scratch::new("cleanup");
    let marker = cwd.path().join("survived");
    let hook = cwd.script(
        "tree.sh",
        &format!("/bin/sh -c 'sleep 2; : > {}'", marker.display()),
    );
    let limits = HookLimits {
        timeout: Duration::from_millis(200),
        max_output: HOOK_OUTPUT_LIMIT,
    };

    run_hook(cwd.path(), Some(&hook), &host_env(&[]), limits)
        .expect_err("the hook overruns its limit");

    // Long enough for the grandchild to have written the marker had it survived.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !marker.exists(),
        "the grandchild outlived the timeout, so the kill did not reach the \
         process group",
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
