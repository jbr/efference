//! The bash tool — Phase 0's only tool (DESIGN §5, §6).
//!
//! Two concerns meet here. The *capability* is trivial: run a shell command,
//! return its combined output. The load-bearing part is *ownership* (DESIGN §6,
//! seeded now): the child is spawned with `process_group(0)` so it leads a fresh
//! process group (child pid == pgid), and [`Bash::run`] races completion against
//! an injected `abort` future — on abort it `killpg`s the whole group with
//! `SIGKILL`, taking down any grandchildren the command spawned.
//!
//! `abort` is injected rather than reaching for `tokio::signal::ctrl_c()` inside
//! so the kill path is unit-testable without signalling the test runner; the
//! agent loop passes `ctrl_c()`. This is the C-c guardrail from the settled
//! interaction model (DESIGN §10): see the command, abort before it finishes.
//!
//! Unix-only: `process_group` / `killpg` are POSIX. Phase 0 targets darwin.

use std::future::Future;
use std::process::{ExitStatus, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::log::RawResult;

/// The tool name the model calls; `tool_use` / `tool_result` ids pair on it.
pub const NAME: &str = "bash";

const DESCRIPTION: &str = "Run a bash command and return its combined \
stdout/stderr. Runs in its own process group; the user approves each call and \
can abort a running command with Ctrl-C.";

/// Phase 0's only tool: run a shell command in its own process group.
///
/// A unit struct for now — it will grow a cwd / session process-group as the
/// supervisor (DESIGN §6) lands, but Phase 0 needs no state.
pub struct Bash;

#[derive(Deserialize)]
struct Input {
    command: String,
}

impl Bash {
    /// The Anthropic `tools[]` entry — name, description, JSON-Schema input.
    /// Built from a [`serde_json::Value`] so its keys serialize in sorted order
    /// (cache determinism, DESIGN §3) when this lands in the request next slice.
    pub fn definition() -> Value {
        json!({
            "name": NAME,
            "description": DESCRIPTION,
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to run."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Run asynchronously: return immediately with a bg#n handle, surface the process's recent output in your inbox at each step, and notify you when it exits. Use for long-running or watch processes (servers, `cargo watch`, test runners) so you can keep working. Omit or false to run to completion and get the output now."
                    }
                },
                "required": ["command"]
            }
        })
    }

    /// Run one command. Always resolves to a [`RawResult`]: a non-zero exit, a
    /// spawn failure, malformed input, or a user abort all come back as
    /// `is_error: true` rather than bubbling up — the loop must always have a
    /// `tool_result` to pair with the `tool_use`, or the API rejects the orphan.
    ///
    /// Streams as it runs: instead of buffering with `wait_with_output`, both pipes
    /// are drained incrementally and each chunk is forwarded to `on_output` (the
    /// transport-only live sink — the daemon publishes it to `GET /stream`) while
    /// the canonical bytes accumulate. Stdout and stderr land in **arrival order**
    /// (a single interleaved buffer), so progress output written to stderr — cargo,
    /// say — streams too; this is also more terminal-faithful than the old
    /// stdout-block-then-stderr-block rendering.
    pub async fn run(
        &self,
        input: &Value,
        abort: impl Future<Output = ()>,
        mut on_output: impl FnMut(&str),
    ) -> RawResult {
        let command = match Input::deserialize(input) {
            Ok(input) => input.command,
            Err(e) => return error(format!("invalid bash input: {e}")),
        };

        let mut child = match Command::new("bash")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Lead a fresh process group so one `killpg` reaps the command and
            // anything it spawns (DESIGN §6). child pid == pgid.
            .process_group(0)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return error(format!("failed to spawn bash: {e}")),
        };

        // Grab the pgid before anything consumes the handle; `killpg` needs only it.
        let pgid = child.id();
        // Take the pipes so we can read them incrementally; both are piped above.
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        // Drain both pipes in arrival order — forwarding each chunk live and
        // accumulating the canonical bytes — until both close or the user aborts.
        // The live view is per-chunk `from_utf8_lossy` (a multi-byte char split at a
        // read boundary shows a transient replacement char — cosmetic); the canonical
        // bytes are decoded once, whole, in [`render`].
        let mut combined: Vec<u8> = Vec::new();
        let cancelled = drain_pipes(stdout, stderr, abort, |chunk| {
            on_output(&String::from_utf8_lossy(chunk));
            combined.extend_from_slice(chunk);
        })
        .await;
        if cancelled {
            if let Some(pgid) = pgid {
                kill_group(pgid);
            }
            let mut content = String::from_utf8_lossy(&combined).into_owned();
            push_line_break(&mut content);
            content.push_str("[aborted by user — process group killed]");
            return RawResult { content, is_error: true };
        }

        // Both pipes are at EOF, so the child has finished writing; reap its status.
        match child.wait().await {
            Ok(status) => render(combined, status),
            Err(e) => error(format!("error waiting on bash: {e}")),
        }
    }
}

/// Drain both pipes in arrival order — forwarding each freshly-read chunk to
/// `on_chunk` — until both close (EOF) or `cancel` fires. Returns `true` iff
/// cancelled. The shared core of the foreground bash tool (which accumulates the
/// chunks and races the user's abort) and the background supervisor's drain task
/// (which feeds each chunk into a process's capture buffer and never cancels — a
/// kill closes the pipes externally, so it ends at EOF). DESIGN §6.
pub(crate) async fn drain_pipes(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    cancel: impl Future<Output = ()>,
    mut on_chunk: impl FnMut(&[u8]),
) -> bool {
    let mut out_buf = [0u8; 8192];
    let mut err_buf = [0u8; 8192];
    let (mut out_done, mut err_done) = (false, false);
    tokio::pin!(cancel);
    while !(out_done && err_done) {
        tokio::select! {
            _ = &mut cancel => return true,
            r = stdout.read(&mut out_buf), if !out_done => match r {
                Ok(0) | Err(_) => out_done = true,
                Ok(n) => on_chunk(&out_buf[..n]),
            },
            r = stderr.read(&mut err_buf), if !err_done => match r {
                Ok(0) | Err(_) => err_done = true,
                Ok(n) => on_chunk(&err_buf[..n]),
            },
        }
    }
    false
}

/// `SIGKILL` the whole process group led by `pgid`. The child was spawned with
/// `process_group(0)`, so its pid *is* its pgid; killing the group takes down
/// any grandchildren too. Best-effort — a racing exit may already have reaped
/// it, in which case `killpg` just returns `ESRCH`. Used by the foreground abort
/// path and by the supervisor's kill / teardown (DESIGN §6).
pub(crate) fn kill_group(pgid: u32) {
    // SAFETY: an FFI call with no memory effects; a stale/invalid pgid is a
    // benign `ESRCH`, which we intentionally ignore (best-effort teardown).
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
}

/// Decode the accumulated (arrival-interleaved) output and mark `is_error` on a
/// non-zero exit, annotating the status (a code, or `signal` when killed by one).
fn render(combined: Vec<u8>, status: ExitStatus) -> RawResult {
    let mut content = String::from_utf8_lossy(&combined).into_owned();

    let is_error = !status.success();
    if is_error {
        let status = status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        push_line_break(&mut content);
        content.push_str(&format!("[exited with status {status}]"));
    }
    if content.is_empty() {
        content.push_str("(no output)");
    }
    RawResult { content, is_error }
}

fn push_line_break(content: &mut String) {
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
}

fn error(content: impl Into<String>) -> RawResult {
    RawResult {
        content: content.into(),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Ready, pending, ready};
    use std::time::Instant;

    /// An `abort` that never fires — the command runs to completion.
    fn never() -> impl Future<Output = ()> {
        pending()
    }

    /// An `abort` that's already resolved — fires on the first poll.
    fn now() -> Ready<()> {
        ready(())
    }

    /// A sink that discards streamed chunks — the unit tests assert the canonical
    /// result; the live-streaming path is exercised separately.
    fn sink() -> impl FnMut(&str) {
        |_| {}
    }

    #[tokio::test]
    async fn runs_command_and_captures_stdout() {
        let result = Bash.run(&json!({ "command": "echo hello" }), never(), sink()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"), "got {:?}", result.content);
    }

    #[tokio::test]
    async fn captures_stderr_without_marking_error() {
        let result = Bash.run(&json!({ "command": "echo oops 1>&2" }), never(), sink()).await;
        assert!(!result.is_error, "exit 0 even though it wrote to stderr");
        assert!(result.content.contains("oops"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_and_surfaces_status() {
        let result = Bash.run(&json!({ "command": "exit 7" }), never(), sink()).await;
        assert!(result.is_error);
        assert!(result.content.contains('7'), "status should surface: {:?}", result.content);
    }

    #[tokio::test]
    async fn malformed_input_is_an_error_result_not_a_panic() {
        let result = Bash.run(&json!({ "not_command": true }), never(), sink()).await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid bash input"));
    }

    /// Abort beats a long-running command: the call returns promptly with an
    /// error and `killpg` reaps the group, rather than blocking for the sleep.
    #[tokio::test]
    async fn abort_kills_a_running_command_promptly() {
        let start = Instant::now();
        let result = Bash.run(&json!({ "command": "sleep 30" }), now(), sink()).await;
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
        let result = Bash
            .run(
                &json!({ "command": "echo one; echo two 1>&2; echo three" }),
                never(),
                |chunk: &str| streamed.push_str(chunk),
            )
            .await;
        assert!(!result.is_error);
        for line in ["one", "two", "three"] {
            assert!(streamed.contains(line), "live stream missing {line}: {streamed:?}");
            assert!(result.content.contains(line), "result missing {line}: {:?}", result.content);
        }
        assert_eq!(streamed, result.content, "streamed bytes must equal the canonical output");
    }
}
