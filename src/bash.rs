//! The bash tool — Phase 0's only tool (docs/tools.md, docs/supervisor.md).
//!
//! Two concerns meet here. The *capability* is trivial: run a shell command,
//! return its combined output. The critical part is *ownership* (docs/supervisor.md,
//! seeded now): the child is spawned with `process_group(0)` so it leads a fresh
//! process group (child pid == pgid), and [`Bash::run`] races completion against
//! an injected `abort` future — on abort it `killpg`s the whole group with
//! `SIGKILL`, taking down any grandchildren the command spawned.
//!
//! `abort` is injected rather than reaching for `tokio::signal::ctrl_c()` inside
//! so the kill path is unit-testable without signalling the test runner; the
//! agent loop passes `ctrl_c()`. This is the C-c guardrail from the settled
//! interaction model (docs/README.md): see the command, abort before it finishes.
//!
//! Unix-only: `process_group` / `killpg` are POSIX. Phase 0 targets darwin.

use std::future::Future;
use std::process::{ExitStatus, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::frontend::Interrupt;
use crate::log::RawResult;

/// What a tool run resolved to. Every path still pairs the `tool_use` with
/// exactly one `tool_result` — a detach just isn't the *tool's* result to
/// produce: the loop adopts the child into the supervisor and writes the
/// handoff result itself (docs/supervisor.md).
pub enum Outcome {
    /// The call finished (success, error, or killed): its one paired result.
    Done(RawResult),
    /// The operator detached the still-running command mid-flight
    /// (docs/supervisor.md, the foreground→background handoff). Carries
    /// everything the supervisor needs to adopt it without losing a byte.
    /// Boxed: the child + pipes + seed dwarf `Done` (clippy::large_enum_variant),
    /// same reasoning as the MCP variants in `tools.rs`.
    Detached(Box<DetachedBash>),
}

impl From<RawResult> for Outcome {
    fn from(result: RawResult) -> Self {
        Outcome::Done(result)
    }
}

/// A live child surrendered by [`Bash::run`] on detach — not reaped, not
/// killed. `seed` is everything drained before the detach won the select;
/// the supervisor's capture is pre-filled with it, and the continuing drain
/// resumes from exactly that byte boundary (no gap, no dup — the select only
/// completes a read after its bytes are in `seed`).
pub struct DetachedBash {
    pub command: String,
    pub child: tokio::process::Child,
    pub stdout: tokio::process::ChildStdout,
    pub stderr: tokio::process::ChildStderr,
    pub seed: Vec<u8>,
}

/// The tool name the model calls; `tool_use` / `tool_result` ids pair on it.
pub const NAME: &str = "bash";

const DESCRIPTION: &str = "Run a bash command and return its combined \
stdout/stderr. Runs in its own process group; the user approves each call and \
can abort a running command with Ctrl-C.";

/// Run a shell command in its own process group.
///
/// `tmpdir`, when set, overrides `$TMPDIR` in the child environment to the
/// session's private scratch directory (docs/tools.md): the ecosystem already
/// respects `$TMPDIR` (`mktemp`, `std::env::temp_dir()`, Python `tempfile`), so
/// incidental *and* deliberate temp files land in the sanctioned, granted,
/// session-cleaned dir with no agent awareness required.
#[derive(Default)]
pub struct Bash {
    pub tmpdir: Option<std::path::PathBuf>,
}

#[derive(Deserialize)]
struct Input {
    command: String,
}

/// Whether a call is a `bash` invocation asking to be backgrounded
/// (docs/supervisor.md) — `{ "command": …, "background": true }`. Such a call is
/// handled structurally by the loop (it registers a child with the supervisor)
/// rather than run to completion here.
pub fn is_background_call(call: &crate::log::ToolCall) -> bool {
    call.name == NAME
        && call
            .input
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Whether a call is a `bash` command that runs *in the foreground* — the only
/// call that is ever the interruptible, detachable running tool. `kill` races
/// [`Bash::run`]'s abort future (no other backend awaits it) and the
/// foreground→background handoff adopts its live child, so the clients that offer
/// those verbs ask this before showing them. Lives beside the schema that defines
/// the `background` flag, so there is one reader of it.
pub fn is_foreground_call(call: &crate::log::ToolCall) -> bool {
    call.name == NAME && !is_background_call(call)
}

impl Bash {
    /// The Anthropic `tools[]` entry — name, description, JSON-Schema input.
    /// Built from a [`serde_json::Value`] so its keys serialize in sorted order
    /// (cache determinism, docs/log-and-projection.md) when this lands in the request next slice.
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

    /// Run one command. Resolves to [`Outcome::Done`] on every path but one: a
    /// non-zero exit, a spawn failure, malformed input, or a user kill all come
    /// back as an `is_error: true` result rather than bubbling up — the loop
    /// must always have a `tool_result` to pair with the `tool_use`, or the API
    /// rejects the orphan. The exception is a detach ([`Interrupt::Detach`]),
    /// which surrenders the live child as [`Outcome::Detached`]; the loop owns
    /// the pairing there too (the handoff result, docs/supervisor.md).
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
        abort: impl Future<Output = Interrupt>,
        mut on_output: impl FnMut(&str),
    ) -> Outcome {
        let command = match Input::deserialize(input) {
            Ok(input) => input.command,
            Err(e) => return error(format!("invalid bash input: {e}")).into(),
        };

        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Lead a fresh process group so one `killpg` reaps the command and
            // anything it spawns (docs/supervisor.md). child pid == pgid.
            .process_group(0);
        // The session scratch dir rides `$TMPDIR` into every child (docs/tools.md).
        if let Some(tmpdir) = &self.tmpdir {
            cmd.env("TMPDIR", tmpdir);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return error(format!("failed to spawn bash: {e}")).into(),
        };

        // Grab the pgid before anything consumes the handle; `killpg` needs only it.
        let pgid = child.id();
        // Take the pipes so we can read them incrementally; both are piped above.
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");

        // Drain both pipes in arrival order — forwarding each chunk live and
        // accumulating the canonical bytes — until both close or the user
        // interrupts. The live view is per-chunk `from_utf8_lossy` (a multi-byte
        // char split at a read boundary shows a transient replacement char —
        // cosmetic); the canonical bytes are decoded once, whole, in [`render`].
        let mut combined: Vec<u8> = Vec::new();
        let interrupted = drain_pipes(&mut stdout, &mut stderr, abort, |chunk| {
            on_output(&String::from_utf8_lossy(chunk));
            combined.extend_from_slice(chunk);
        })
        .await;
        match interrupted {
            Some(Interrupt::Kill) => {
                if let Some(pgid) = pgid {
                    kill_group(pgid);
                }
                let mut content = String::from_utf8_lossy(&combined).into_owned();
                push_line_break(&mut content);
                content.push_str("[aborted by user — process group killed]");
                RawResult {
                    content,
                    is_error: true,
                }
                .into()
            }
            // Surrender the live child, don't reap it (docs/supervisor.md): no
            // `killpg`, no `wait`. The loop adopts it into the supervisor, seeding
            // the capture with everything already drained.
            Some(Interrupt::Detach) => Outcome::Detached(Box::new(DetachedBash {
                command,
                child,
                stdout,
                stderr,
                seed: combined,
            })),
            // Both pipes at EOF: the child has finished writing; reap its status.
            None => match child.wait().await {
                Ok(status) => render(combined, status).into(),
                Err(e) => error(format!("error waiting on bash: {e}")).into(),
            },
        }
    }
}

/// Drain both pipes in arrival order — forwarding each freshly-read chunk to
/// `on_chunk` — until both close (EOF, returning `None`) or `cancel` fires
/// (returning its value: the interrupt intent). The pipes are borrowed, not
/// consumed, so a detach can hand them onward with every pre-cancel byte already
/// delivered — a read only consumes bytes when its poll completes, so cancelling
/// mid-read loses nothing. The shared core of the foreground bash tool (which
/// accumulates the chunks and races the user's interrupt) and the background
/// supervisor's drain task (which feeds each chunk into a process's capture
/// buffer and never cancels — a kill closes the pipes externally, so it ends at
/// EOF). See docs/supervisor.md.
pub(crate) async fn drain_pipes<T>(
    stdout: &mut tokio::process::ChildStdout,
    stderr: &mut tokio::process::ChildStderr,
    cancel: impl Future<Output = T>,
    mut on_chunk: impl FnMut(&[u8]),
) -> Option<T> {
    let mut out_buf = [0u8; 8192];
    let mut err_buf = [0u8; 8192];
    let (mut out_done, mut err_done) = (false, false);
    tokio::pin!(cancel);
    while !(out_done && err_done) {
        tokio::select! {
            intent = &mut cancel => return Some(intent),
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
    None
}

/// `SIGKILL` the whole process group led by `pgid`. The child was spawned with
/// `process_group(0)`, so its pid *is* its pgid; killing the group takes down
/// any grandchildren too. Best-effort — a racing exit may already have reaped
/// it, in which case `killpg` just returns `ESRCH`. Used by the foreground abort
/// path and by the supervisor's kill / teardown (docs/supervisor.md).
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
mod test;
