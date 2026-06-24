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
use std::process::{Output, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};
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
    pub async fn run(&self, input: &Value, abort: impl Future<Output = ()>) -> RawResult {
        let command = match Input::deserialize(input) {
            Ok(input) => input.command,
            Err(e) => return error(format!("invalid bash input: {e}")),
        };

        let child = match Command::new("bash")
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

        // Grab the pgid before `wait_with_output` consumes the handle; on the
        // abort branch the handle is gone, but the pid is all `killpg` needs.
        let pgid = child.id();

        tokio::select! {
            result = child.wait_with_output() => match result {
                Ok(output) => render(output),
                Err(e) => error(format!("error waiting on bash: {e}")),
            },
            _ = abort => {
                if let Some(pgid) = pgid {
                    kill_group(pgid);
                }
                error("aborted by user — process group killed")
            }
        }
    }
}

/// `SIGKILL` the whole process group led by `pgid`. The child was spawned with
/// `process_group(0)`, so its pid *is* its pgid; killing the group takes down
/// any grandchildren too. Best-effort — a racing exit may already have reaped
/// it, in which case `killpg` just returns `ESRCH`.
fn kill_group(pgid: u32) {
    // SAFETY: an FFI call with no memory effects; a stale/invalid pgid is a
    // benign `ESRCH`, which we intentionally ignore (best-effort teardown).
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
}

/// Combine stdout then stderr; mark `is_error` on non-zero exit and annotate the
/// status (a code, or `signal` when the process was killed by one).
fn render(output: Output) -> RawResult {
    let mut content = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        push_line_break(&mut content);
        content.push_str(&stderr);
    }

    let is_error = !output.status.success();
    if is_error {
        let status = output
            .status
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

    #[tokio::test]
    async fn runs_command_and_captures_stdout() {
        let result = Bash.run(&json!({ "command": "echo hello" }), never()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"), "got {:?}", result.content);
    }

    #[tokio::test]
    async fn captures_stderr_without_marking_error() {
        let result = Bash.run(&json!({ "command": "echo oops 1>&2" }), never()).await;
        assert!(!result.is_error, "exit 0 even though it wrote to stderr");
        assert!(result.content.contains("oops"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_and_surfaces_status() {
        let result = Bash.run(&json!({ "command": "exit 7" }), never()).await;
        assert!(result.is_error);
        assert!(result.content.contains('7'), "status should surface: {:?}", result.content);
    }

    #[tokio::test]
    async fn malformed_input_is_an_error_result_not_a_panic() {
        let result = Bash.run(&json!({ "not_command": true }), never()).await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid bash input"));
    }

    /// Abort beats a long-running command: the call returns promptly with an
    /// error and `killpg` reaps the group, rather than blocking for the sleep.
    #[tokio::test]
    async fn abort_kills_a_running_command_promptly() {
        let start = Instant::now();
        let result = Bash.run(&json!({ "command": "sleep 30" }), now()).await;
        assert!(result.is_error);
        assert!(result.content.contains("aborted"));
        assert!(start.elapsed().as_secs() < 5, "must not wait out the sleep");
    }
}
