
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
fn sink() -> impl FnMut(&str) {
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
            .run(&json!({ "command": "echo hello" }), never(), sink())
            .await,
    );
    assert!(!result.is_error);
    assert!(result.content.contains("hello"), "got {:?}", result.content);
}

#[tokio::test]
async fn captures_stderr_without_marking_error() {
    let result = done(
        Bash::default()
            .run(&json!({ "command": "echo oops 1>&2" }), never(), sink())
            .await,
    );
    assert!(!result.is_error, "exit 0 even though it wrote to stderr");
    assert!(result.content.contains("oops"));
}

#[tokio::test]
async fn nonzero_exit_is_error_and_surfaces_status() {
    let result = done(
        Bash::default()
            .run(&json!({ "command": "exit 7" }), never(), sink())
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
            .run(&json!({ "not_command": true }), never(), sink())
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
            .run(&json!({ "command": "sleep 30" }), now(), sink())
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
            |chunk: &str| streamed.push_str(chunk),
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
    };
    let result = done(
        bash.run(&json!({ "command": "echo $TMPDIR" }), never(), sink())
            .await,
    );
    assert_eq!(result.content.trim(), "/scratch/efference-test");
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
