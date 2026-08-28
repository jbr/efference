use super::*;
use std::future::{Ready, pending, ready};
use std::time::{Duration, Instant};

/// An `abort` that never fires — the command runs to completion.
fn never() -> impl Future<Output = Interrupt> {
    pending()
}

/// A kill that's already resolved — fires on the first poll.
fn now() -> Ready<Interrupt> {
    ready(Interrupt::Kill)
}

/// A sink that discards streamed chunks — the unit tests assert the canonical
/// result; the live-streaming path is exercised separately.
fn sink() -> impl FnMut(&[u8]) {
    |_| {}
}

/// A spawn hook that ignores the pid — the registration path is the daemon's.
fn no_spawn() -> impl FnOnce(Option<u32>) {
    |_| {}
}

/// Unwrap the common case; the detach test matches [`Outcome::Detached`] itself.
fn done(outcome: Outcome) -> RawResult {
    match outcome {
        Outcome::Done(result) => result,
        Outcome::Detached(_) => panic!("expected a completed result, got a detach"),
    }
}

#[tokio::test]
async fn runs_command_and_captures_stdout() {
    let result = done(
        Bash::default()
            .run(
                &json!({ "command": "echo hello" }),
                never(),
                no_spawn(),
                sink(),
            )
            .await,
    );
    assert!(!result.is_error);
    assert!(result.content.contains("hello"), "got {:?}", result.content);
}

#[tokio::test]
async fn captures_stderr_without_marking_error() {
    let result = done(
        Bash::default()
            .run(
                &json!({ "command": "echo oops 1>&2" }),
                never(),
                no_spawn(),
                sink(),
            )
            .await,
    );
    assert!(!result.is_error, "exit 0 even though it wrote to stderr");
    assert!(result.content.contains("oops"));
}

#[tokio::test]
async fn nonzero_exit_is_error_and_surfaces_status() {
    let result = done(
        Bash::default()
            .run(&json!({ "command": "exit 7" }), never(), no_spawn(), sink())
            .await,
    );
    assert!(result.is_error);
    assert!(
        result.content.contains('7'),
        "status should surface: {:?}",
        result.content
    );
}

#[tokio::test]
async fn malformed_input_is_an_error_result_not_a_panic() {
    let result = done(
        Bash::default()
            .run(&json!({ "not_command": true }), never(), no_spawn(), sink())
            .await,
    );
    assert!(result.is_error);
    assert!(result.content.contains("invalid bash input"));
}

/// Abort beats a long-running command: the call returns promptly with an
/// error and `killpg` reaps the group, rather than blocking for the sleep.
#[tokio::test]
async fn abort_kills_a_running_command_promptly() {
    let start = Instant::now();
    let result = done(
        Bash::default()
            .run(&json!({ "command": "sleep 30" }), now(), no_spawn(), sink())
            .await,
    );
    assert!(result.is_error);
    assert!(result.content.contains("aborted"));
    assert!(start.elapsed().as_secs() < 5, "must not wait out the sleep");
}

/// Each chunk reaches the live sink as it is read — and the concatenation of the
/// streamed chunks matches the canonical output (modulo the trailing status
/// note), proving the stream and the committed result agree.
#[tokio::test]
async fn streams_chunks_to_the_sink() {
    let mut streamed = String::new();
    let outcome = Bash::default()
        .run(
            &json!({ "command": "echo one; echo two 1>&2; echo three" }),
            never(),
            no_spawn(),
            |chunk: &[u8]| streamed.push_str(&String::from_utf8_lossy(chunk)),
        )
        .await;
    let result = done(outcome);
    assert!(!result.is_error);
    for line in ["one", "two", "three"] {
        assert!(
            streamed.contains(line),
            "live stream missing {line}: {streamed:?}"
        );
        assert!(
            result.content.contains(line),
            "result missing {line}: {:?}",
            result.content
        );
    }
    assert_eq!(
        streamed, result.content,
        "streamed bytes must equal the canonical output"
    );
}

/// The session scratch dir overrides `$TMPDIR` in the child environment
/// (docs/tools.md) — and without one, the inherited value is untouched.
#[tokio::test]
async fn tmpdir_is_overridden_when_set() {
    let bash = Bash {
        tmpdir: Some(std::path::PathBuf::from("/scratch/efference-test")),
        ..Bash::default()
    };
    let result = done(
        bash.run(
            &json!({ "command": "echo $TMPDIR" }),
            never(),
            no_spawn(),
            sink(),
        )
        .await,
    );
    assert_eq!(result.content.trim(), "/scratch/efference-test");
}

/// A variable the `[env]` table marks `strip = true` is absent from the child's
/// environment, and one it does not is inherited (docs/env.md). The 2×2's spawn
/// half, checked where it actually happens.
#[tokio::test]
async fn stripped_variables_do_not_reach_the_child() {
    // SAFETY: names unique to this test; the process env is read at spawn.
    unsafe {
        std::env::set_var("EFFERENCE_TEST_STRIPPED", "gone");
        std::env::set_var("EFFERENCE_TEST_KEPT", "here");
    }
    let bash = Bash {
        child_env: crate::env_table::ChildEnv::new(vec!["EFFERENCE_TEST_STRIPPED".to_string()]),
        ..Bash::default()
    };
    let result = done(
        bash.run(
            &json!({ "command": "echo \"[${EFFERENCE_TEST_STRIPPED-unset}][$EFFERENCE_TEST_KEPT]\"" }),
            never(),
            no_spawn(),
            sink(),
        )
        .await,
    );
    assert_eq!(result.content.trim(), "[unset][here]");
    unsafe {
        std::env::remove_var("EFFERENCE_TEST_STRIPPED");
        std::env::remove_var("EFFERENCE_TEST_KEPT");
    }
}

/// A value the `[env]` table names — a literal, or a reference the harness
/// resolved at boot — is set into the child's environment (docs/env.md). The
/// other half of the spawn-time question: strip removes, a value injects, and a
/// name that is somehow both ends up with the value.
#[tokio::test]
async fn named_values_are_injected_into_the_child() {
    let child_env = crate::env_table::ChildEnv::new(vec!["EFFERENCE_TEST_INJECTED".to_string()])
        .with_inject(
            [(
                "EFFERENCE_TEST_INJECTED".to_string(),
                "resolved-value".to_string(),
            )]
            .into_iter()
            .collect(),
        );
    let bash = Bash {
        child_env,
        ..Bash::default()
    };
    let result = done(
        bash.run(
            &json!({ "command": "echo \"[${EFFERENCE_TEST_INJECTED-unset}]\"" }),
            never(),
            no_spawn(),
            sink(),
        )
        .await,
    );
    assert_eq!(result.content.trim(), "[resolved-value]");
}

/// A detach surrenders the live child instead of killing it: the outcome
/// carries the already-drained bytes as the seed, the child is still
/// running, and the surrendered pipes keep delivering output produced
/// *after* the detach — the no-gap/no-dup seam the handoff depends on
/// (docs/supervisor.md).
#[tokio::test]
async fn detach_surrenders_a_live_child_with_its_output_so_far() {
    let detach = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Interrupt::Detach
    };
    let outcome = Bash::default()
        .run(
            &json!({ "command": "echo early; sleep 0.6; echo late" }),
            detach,
            no_spawn(),
            sink(),
        )
        .await;
    let mut detached = match outcome {
        Outcome::Detached(d) => d,
        Outcome::Done(r) => panic!("expected a detach, got {:?}", r.content),
    };
    assert!(
        String::from_utf8_lossy(&detached.seed).contains("early"),
        "seed must carry pre-detach output: {:?}",
        String::from_utf8_lossy(&detached.seed)
    );
    assert_eq!(detached.command, "echo early; sleep 0.6; echo late");
    let pgid = detached.child.id().expect("child still running");

    // The surrendered pipes continue from the seed's byte boundary: draining
    // them to EOF yields the post-detach output exactly once.
    let mut rest: Vec<u8> = Vec::new();
    drain_pipes(
        &mut detached.stdout,
        &mut detached.stderr,
        pending::<()>(),
        |chunk| rest.extend_from_slice(chunk),
    )
    .await;
    let rest = String::from_utf8_lossy(&rest);
    assert!(
        rest.contains("late"),
        "post-detach output continues: {rest:?}"
    );
    assert!(
        !rest.contains("early"),
        "no duplicated pre-detach bytes: {rest:?}"
    );
    detached.child.wait().await.expect("child exits cleanly");
    // Best-effort cleanup if anything lingers in the group.
    kill_group(pgid);
}

/// **A cache-prefix pin.** `tools[]` sits in the request envelope ahead of
/// `messages`, so any byte change to a tool definition invalidates every cached
/// prefix in production, silently (CLAUDE.md, docs/log-and-projection.md). The
/// envelope pin in `api/test.rs` fixes where `tools` sits and how it encodes,
/// but it does so against a *synthetic* one-tool array — so it does not see the
/// real definitions, and a schema edit here used to pass every test.
///
/// If this fails, the change moves the production cache prefix. That may be
/// entirely intended (it was, when `background_after` was added) — update the
/// expectation deliberately, don't reflexively.
#[test]
fn definition_matches_expected_wire_bytes() {
    let expected = concat!(
        r#"{"description":"Run a bash command and return its combined stdout/stderr. Runs in its own process group; the user approves each call and can abort a running command with Ctrl-C.","#,
        r#""input_schema":{"properties":{"#,
        r#""background":{"description":"Run asynchronously: return immediately with a p#n handle, surface the process's recent output in your inbox at each step, and notify you when it exits. Use for long-running or watch processes (servers, `cargo watch`, test runners) so you can keep working. Omit or false to run to completion and get the output now. Note that backgrounding inside the command with a shell `&` does NOT do this — the command will not return, because the process it starts keeps the call's output open.","type":"boolean"},"#,
        r#""background_after":{"description":"Seconds after which this call stops blocking and moves to the background on its own, as if you had set `background: true` from the start. Nothing is killed and no output is lost — you get a p#n handle and can read_process it. This is an upper bound you expect NOT to hit: set it when you know a command is legitimately slow (a big build), and leave it out otherwise to take the default. Exceeding it is a signal that something is wrong, not a failure.","type":"integer"},"#,
        r#""command":{"description":"The bash command to run.","type":"string"}"#,
        r#"},"required":["command"],"type":"object"},"name":"bash"}"#,
    );
    assert_eq!(
        serde_json::to_string(&Bash::definition()).unwrap(),
        expected
    );
}

/// A `Bash` whose deadline fires almost immediately, for the timer tests.
fn impatient(after: Duration, max: Duration) -> Bash {
    Bash {
        child_env: crate::env_table::ChildEnv::default(),
        tmpdir: None,
        background_after: after,
        background_after_max: max,
    }
}

/// **The loop-safety property.** A foreground command that outlives its deadline
/// stops blocking the turn: it detaches into the supervisor exactly as an
/// operator detach does, rather than parking the loop forever. Before this, a
/// command whose grandchild held the pipes open never finalized and only an
/// operator could recover it — which an autonomous session has none of
/// (docs/supervisor.md).
#[tokio::test]
async fn a_command_past_its_deadline_detaches_instead_of_blocking() {
    let outcome = impatient(Duration::from_millis(50), Duration::from_secs(60))
        .run(
            &json!({ "command": "sleep 30" }),
            never(),
            no_spawn(),
            sink(),
        )
        .await;
    match outcome {
        Outcome::Detached(d) => {
            assert_eq!(
                d.auto_after,
                Some(Duration::from_millis(50)),
                "the timer detached it, so the handoff must be able to say so"
            );
        }
        Outcome::Done(r) => panic!("expected a detach, got: {}", r.content),
    }
}

/// The deadline must not truncate work that finishes inside it — the timer is a
/// backstop, not a budget.
#[tokio::test]
async fn a_command_inside_its_deadline_is_unaffected() {
    let result = done(
        impatient(Duration::from_secs(30), Duration::from_secs(60))
            .run(
                &json!({ "command": "echo quick" }),
                never(),
                no_spawn(),
                sink(),
            )
            .await,
    );
    assert_eq!(result.content.trim(), "quick");
    assert!(!result.is_error);
}

/// A call's own `background_after` raises its deadline — the point of the field
/// is that a genuinely slow build can ask for longer than the default.
#[tokio::test]
async fn a_call_can_raise_its_own_deadline_above_the_default() {
    let result = done(
        impatient(Duration::from_millis(50), Duration::from_secs(60))
            .run(
                &json!({ "command": "sleep 0.4 && echo survived", "background_after": 30 }),
                never(),
                no_spawn(),
                sink(),
            )
            .await,
    );
    assert_eq!(
        result.content.trim(),
        "survived",
        "the call asked for 30s and must not be detached at the 50ms default"
    );
}

/// ...but only up to the operator's ceiling. The clamp is the property that has
/// to hold at exactly the moment the caller's estimate is wrong, so a call
/// cannot raise itself past it — an agent that pads defensively, or simply
/// guesses badly, must not be able to park the turn.
#[tokio::test]
async fn a_call_cannot_raise_its_deadline_past_the_operator_ceiling() {
    let outcome = impatient(Duration::from_millis(50), Duration::from_millis(80))
        .run(
            &json!({ "command": "sleep 30", "background_after": 3600 }),
            never(),
            no_spawn(),
            sink(),
        )
        .await;
    match outcome {
        Outcome::Detached(d) => assert_eq!(
            d.auto_after,
            Some(Duration::from_millis(80)),
            "clamped to the ceiling, not the requested hour"
        ),
        Outcome::Done(r) => panic!("the ceiling must still detach it: {}", r.content),
    }
}

/// An operator detach is still reported as an operator detach — the timer must
/// not relabel it, since the two read differently to the agent.
#[tokio::test]
async fn an_operator_detach_is_not_labelled_automatic() {
    let outcome = Bash::default()
        .run(
            &json!({ "command": "sleep 30" }),
            ready(Interrupt::Detach),
            no_spawn(),
            sink(),
        )
        .await;
    match outcome {
        Outcome::Detached(d) => assert_eq!(d.auto_after, None),
        Outcome::Done(r) => panic!("expected a detach: {}", r.content),
    }
}

/// The shape that started this: a command that backgrounds a server with a shell
/// `&` leaves a grandchild holding the call's stdout open, so the call never
/// reaches EOF. Observed three times across two models. It must now detach
/// rather than hang forever.
#[tokio::test]
async fn a_shell_backgrounded_grandchild_no_longer_hangs_the_call() {
    // The deadline is generous because the seed assertion below needs the shell
    // to have been scheduled and printed before it fires: at 150ms this failed
    // under full-suite load with the output legitimately not yet produced.
    let outcome = impatient(Duration::from_millis(1000), Duration::from_secs(60))
        .run(
            &json!({ "command": "sleep 30 & echo started; wait" }),
            never(),
            no_spawn(),
            sink(),
        )
        .await;
    match outcome {
        Outcome::Detached(d) => {
            assert!(d.auto_after.is_some());
            assert!(
                String::from_utf8_lossy(&d.seed).contains("started"),
                "output produced before the deadline is preserved, not lost"
            );
        }
        Outcome::Done(r) => panic!("expected a detach, got: {}", r.content),
    }
}
