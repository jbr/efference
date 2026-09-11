//! The bash tool (docs/tools.md, docs/supervisor.md).
//!
//! Two concerns meet here. The *capability* is trivial: run a shell command,
//! return its combined output. The critical part is *ownership*
//! (docs/supervisor.md): the child is spawned with `process_group(0)` so it leads a fresh
//! process group (child pid == pgid), and [`Bash::run`] races completion against
//! an injected `abort` future — on abort it `killpg`s the whole group with
//! `SIGKILL`, taking down any grandchildren the command spawned.
//!
//! `abort` is injected rather than reaching for `tokio::signal::ctrl_c()` inside
//! so the kill path is unit-testable without signalling the test runner; the
//! agent loop passes `ctrl_c()`. This is the C-c guardrail from the settled
//! interaction model (docs/README.md): see the command, abort before it finishes.
//!
//! Unix-only: `process_group` / `killpg` are POSIX.

use std::future::Future;
use std::process::{ExitStatus, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::frontend::Interrupt;
use crate::log::RawResult;
use std::time::Duration;

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
    /// `Some(deadline)` when the *timer* detached this call rather than the
    /// operator — the loop words the handoff differently, because an
    /// auto-detach is the only one the agent did not ask for and is the moment
    /// `background_after` becomes worth knowing about (docs/supervisor.md).
    pub auto_after: Option<Duration>,
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
pub struct Bash {
    /// Working root set directly on each child (docs/tools.md).
    pub cwd: std::path::PathBuf,
    pub tmpdir: Option<std::path::PathBuf>,
    /// Applies when a call names no `background_after` of its own.
    pub background_after: Duration,
    /// The clamp on a call-supplied `background_after`. This is the loop-safety
    /// property — no foreground call may park the turn indefinitely — so it is
    /// deliberately not something a call can raise (docs/supervisor.md).
    pub background_after_max: Duration,
    /// The `[env]` table's spawn-time answer (docs/env.md): variables removed
    /// from this child's environment, and the values the table names set into
    /// it. Standalone defaults capture the local environment; session boot
    /// supplies the caller's snapshot and resolved policy.
    pub child_env: crate::env_table::ChildEnv,
}

/// The shipped defaults, used when no `[bash]` config section says otherwise.
/// 300s clears a real `cargo build --release` while firing on under 1% of this
/// repo's corpus; the ceiling is high enough for a genuinely slow build and
/// still finite. The asymmetry is what sets them: a premature detach costs one
/// `read_process` and loses nothing, an absent one costs the session.
pub const DEFAULT_BACKGROUND_AFTER: Duration = Duration::from_secs(300);
pub const DEFAULT_BACKGROUND_AFTER_MAX: Duration = Duration::from_secs(1800);

impl Default for Bash {
    fn default() -> Self {
        Self {
            cwd: std::path::PathBuf::from("."),
            tmpdir: None,
            background_after: DEFAULT_BACKGROUND_AFTER,
            background_after_max: DEFAULT_BACKGROUND_AFTER_MAX,
            child_env: crate::env_table::ChildEnv::default(),
        }
    }
}

#[derive(Deserialize)]
struct Input {
    command: String,
    /// The caller's own expectation of how long this should take, in seconds.
    /// An expectation, not a promise: exceeding it detaches, and that is a
    /// signal the caller asked for rather than a failure.
    #[serde(default)]
    background_after: Option<u64>,
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
                        "description": "Run asynchronously: return immediately with a p#n handle, surface the process's recent output in your inbox at each step, and notify you when it exits. Use for long-running or watch processes (servers, `cargo watch`, test runners) so you can keep working. Omit or false to run to completion and get the output now. Note that backgrounding inside the command with a shell `&` does NOT do this — the command will not return, because the process it starts keeps the call's output open."
                    },
                    "background_after": {
                        "type": "integer",
                        "description": "Seconds after which this call stops blocking and moves to the background on its own, as if you had set `background: true` from the start. Nothing is killed and no output is lost — you get a p#n handle and can read_process it. This is an upper bound you expect NOT to hit: set it when you know a command is legitimately slow (a big build), and leave it out otherwise to take the default. Exceeding it is a signal that something is wrong, not a failure."
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
    /// `on_spawn` fires once with the child's pid the moment the spawn succeeds —
    /// the hook the daemon uses to register the foreground process in the
    /// supervisor's namespace so it is addressable from spawn (docs/supervisor.md).
    ///
    /// Streams as it runs: instead of buffering with `wait_with_output`, both pipes
    /// are drained incrementally and each raw chunk is forwarded to `on_output` (the
    /// transport-only live sink — the daemon publishes it as `output` frames and
    /// feeds the supervisor's capture) while the canonical bytes accumulate. Stdout
    /// and stderr land in **arrival order** (a single interleaved buffer), so
    /// progress output written to stderr — cargo, say — streams too; this is also
    /// more terminal-faithful than the old stdout-block-then-stderr-block rendering.
    pub async fn run(
        &self,
        input: &Value,
        abort: impl Future<Output = Interrupt>,
        on_spawn: impl FnOnce(Option<u32>),
        mut on_output: impl FnMut(&[u8]),
    ) -> Outcome {
        let (command, background_after) = match Input::deserialize(input) {
            // A call-supplied bound is clamped, never trusted: the ceiling is the
            // property that has to hold at exactly the moment the caller's estimate
            // turns out wrong (docs/supervisor.md).
            Ok(input) => (
                input.command,
                input
                    .background_after
                    .map_or(self.background_after, Duration::from_secs)
                    .min(self.background_after_max),
            ),
            Err(e) => return RawResult::error(format!("invalid bash input: {e}")).into(),
        };

        let mut cmd = Command::new("bash");
        cmd.current_dir(&self.cwd)
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Lead a fresh process group so one `killpg` reaps the command and
            // anything it spawns (docs/supervisor.md). child pid == pgid.
            .process_group(0);
        self.child_env.apply_tokio(&mut cmd);
        // The session scratch dir rides `$TMPDIR` into every child (docs/tools.md).
        if let Some(tmpdir) = &self.tmpdir {
            cmd.env("TMPDIR", tmpdir);
        }
        // The machine root never reaches a tool child (docs/web-bind.md): with it, a
        // child derives the bearer and drives its own approval gates. Hardcoded
        // self-surface protection, deliberately not an `[env]`-style policy knob.
        cmd.env_remove(crate::auth::ROOT_ENV);
        let mut child = match crate::spawn::tokio(&mut cmd) {
            Ok(child) => child,
            Err(e) => return RawResult::error(format!("failed to spawn bash: {e}")).into(),
        };

        // Grab the pgid before anything consumes the handle; `killpg` needs only it.
        let pgid = child.id();
        on_spawn(pgid);
        // Take the pipes so we can read them incrementally; both are piped above.
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");

        // Drain both pipes in arrival order — forwarding each raw chunk live and
        // accumulating the canonical bytes — until both close or the user
        // interrupts. The canonical bytes are decoded once, whole, in [`render`];
        // per-chunk decoding is the sink's concern.
        let mut combined: Vec<u8> = Vec::new();
        // The deadline rides the same cancel slot as the operator's interrupt, so a
        // timer detach and an operator detach are the *same* path — `Interrupt::Detach`
        // surrenders the live child, and everything downstream is unchanged. Whichever
        // fires first wins; `auto` records which it was, for the handoff wording.
        let auto = std::cell::Cell::new(false);
        let deadline = async {
            tokio::time::sleep(background_after).await;
            auto.set(true);
            Interrupt::Detach
        };
        let cancel = async {
            tokio::select! {
                intent = abort => intent,
                intent = deadline => intent,
            }
        };
        let interrupted = drain_pipes(&mut stdout, &mut stderr, cancel, |chunk| {
            on_output(chunk);
            combined.extend_from_slice(chunk);
        })
        .await;
        let auto_after = auto.get().then_some(background_after);
        match interrupted {
            Some(Interrupt::Kill) => {
                if let Some(pgid) = pgid {
                    kill_group(pgid);
                }
                let mut content = String::from_utf8_lossy(&combined).into_owned();
                push_line_break(&mut content);
                content.push_str("[aborted by user — process group killed]");
                RawResult::error(content).into()
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
                auto_after,
            })),
            // Both pipes at EOF: the child has finished writing; reap its status.
            None => match child.wait().await {
                Ok(status) => render(combined, status).into(),
                Err(e) => RawResult::error(format!("error waiting on bash: {e}")).into(),
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

/// The marker [`render`] appends on a non-zero exit. Named so the writer below
/// and [`reported_exit_status`] cannot drift.
const EXITED_WITH: &str = "[exited with status ";

/// The exit status a rendered bash result reports, or `None` when it reports
/// none — which, for a completed foreground command, means it exited zero
/// (docs/tools.md, "Per-call telemetry").
///
/// A string rather than an integer because the marker carries `signal` for a
/// command killed by one, and a number that had to invent a value for that case
/// would be less true than the token. Read back out of the result text on
/// purpose: the exit status never left this module in structured form, and
/// widening `Outcome` to carry it would touch every backend that returns one for
/// a field only telemetry reads. Parser and writer sit together so the format is
/// one thing.
pub(crate) fn reported_exit_status(content: &str) -> Option<&str> {
    content
        .rsplit_once(EXITED_WITH)
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(status, _)| status)
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
        content.push_str(&format!("{EXITED_WITH}{status}]"));
    }
    if content.is_empty() {
        content.push_str("(no output)");
    }
    RawResult::new(content, is_error)
}

fn push_line_break(content: &mut String) {
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
}

#[cfg(test)]
mod test;
