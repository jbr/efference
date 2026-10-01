//! Establishing a session's launch environment at a directory (docs/env.md).
//!
//! The **launch environment** is the base a session's `[env]` policy is applied
//! to — what the operator's shell, `direnv`, or version manager had put in the
//! environment for one directory. Two sources, and the user-wide `env_hook`
//! chooses between them:
//!
//! - **No hook**: the host environment, read directly. Nothing is spawned, and
//!   no shell startup is reconstructed. A host started as a service has whatever
//!   environment its service manager gave it, so this baseline is sparse by
//!   construction; it is not a loaded project environment.
//! - **A hook**: an argv, invoked **directly** with the chosen cwd set on the
//!   subprocess — never a shell `cd`, never a change to the host process's cwd.
//!   It inherits the host environment, so it can reach `mise`, `direnv`, or the
//!   user's own binaries, and reconstructing a richer environment is the
//!   configured command's responsibility. Its **stdout is the complete
//!   environment**, as NUL-delimited `KEY=value` records split at the first `=`
//!   — what `env -0` writes.
//!
//! Hook output **replaces** the base rather than merging into it: a variable the
//! host had and the hook did not report is absent from the session's
//! environment, and one the hook reported is present even if the host never had
//! it. The session's existing `[env]` strip/mask policy is applied afterward, to
//! whatever this returns.
//!
//! Every failure here is a failure. A hook that cannot be run, that exits
//! nonzero, that overruns its bounds, or that writes something other than an
//! environment produces an error and no base — nothing falls back to the host
//! environment, because a session silently launched with the wrong environment
//! is the case this mechanism exists to remove.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use std::os::unix::process::CommandExt;

use super::EnvSnapshot;

/// How long a hook may run before it is killed. A hook is a startup script, so
/// this allows for shell initialization and a version manager's shims; it is
/// finite because creation is synchronous behind it. Unlike a reference
/// resolution — which waits on a human unlocking a vault, and so gets
/// [`crate::env_table::RESOLVE_TIMEOUT`] — a hook has no stdin and so has
/// nothing to wait for.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of a hook's stdout is read. A shell environment is kilobytes; a
/// megabyte is far past any real dump, and the bound is what stops a runaway
/// process from exhausting the host's memory through one creation.
pub const HOOK_OUTPUT_LIMIT: usize = 1024 * 1024;

/// How much of the hook's stderr is drained. It is drained and **discarded**,
/// never read into an error: a startup script may print secrets (a token it
/// fetched, a path it interpolated), and unlike a vault's diagnostic — where the
/// operator configured the command and the message is what they need — this text
/// would reach a log and an API response with no masking applied to it yet.
const STDERR_LIMIT: usize = 8 * 1024;

/// How often the wait loop asks whether the hook has finished.
const POLL: Duration = Duration::from_millis(20);

/// The environment a session launched at `cwd` should carry: the hook's, or the
/// host's when no hook is configured.
///
/// `host_env` is the host's own environment, taken as an argument rather than
/// captured here, so the baseline a launch inherits is the caller's to name —
/// and so both branches can be driven without touching the process environment.
pub fn environment_at(
    cwd: &Path,
    hook: Option<&[String]>,
    host_env: &EnvSnapshot,
) -> Result<EnvSnapshot, LaunchError> {
    run_hook(cwd, hook, host_env, HookLimits::default())
}

/// The bounds one hook run is held to. The defaults are the documented limits; a
/// test lowers both so it need not wait 30 seconds or write a megabyte.
#[derive(Debug, Clone, Copy)]
struct HookLimits {
    timeout: Duration,
    max_output: usize,
}

impl Default for HookLimits {
    fn default() -> Self {
        Self {
            timeout: HOOK_TIMEOUT,
            max_output: HOOK_OUTPUT_LIMIT,
        }
    }
}

/// [`environment_at`] with the bounds spelled out — the seam that lets the
/// timeout and output-limit tests run in milliseconds.
fn run_hook(
    cwd: &Path,
    hook: Option<&[String]>,
    host_env: &EnvSnapshot,
    limits: HookLimits,
) -> Result<EnvSnapshot, LaunchError> {
    assert!(
        cwd.is_absolute(),
        "a launch environment is established at an absolute cwd, so a relative one \
         would resolve against the host's own working directory instead",
    );
    let Some(argv) = hook else {
        return Ok(host_env.clone());
    };
    let (program, args) = argv.split_first().ok_or(LaunchError::NoProgram)?;
    let program = program.clone();

    let mut command = Command::new(&program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The baseline is replaced wholesale rather than inherited, so the hook sees
    // exactly what the host has and nothing of this process's own.
    host_env.apply(&mut command);
    // A shell recipe has children of its own, and they must not outlive a hook
    // that overran. A new process group makes the single kill below reach all of
    // them; `child.kill()` alone would leave them running.
    command.process_group(0);

    let mut child = crate::spawn::std(&mut command).map_err(|error| LaunchError::Spawn {
        program: program.clone(),
        detail: error.to_string(),
    })?;

    let named = program.clone();
    let outcome = std::thread::scope(|scope| -> Result<Vec<u8>, LaunchError> {
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let out = scope.spawn(move || read_capped(&mut stdout, limits.max_output));
        let _err = scope.spawn(move || read_capped(&mut stderr, STDERR_LIMIT));

        // Both pipes are drained on their own threads, so a hook that fills one
        // cannot deadlock against this wait.
        let deadline = Instant::now() + limits.timeout;
        let stopped = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Stopped::Exited(status),
                Ok(None) => {}
                Err(error) => break Stopped::Wait(error.to_string()),
            }
            // A reader that finished has seen everything the hook will write, and
            // a hook blocked on a full pipe would otherwise sit here until the
            // deadline. The reader's result below distinguishes the two reasons.
            if out.is_finished() {
                // Usually the hook closed stdout on its way out and the exit is a
                // poll behind; take it if it has landed.
                break match child.try_wait() {
                    Ok(Some(status)) => Stopped::Exited(status),
                    _ => Stopped::ReadFinished,
                };
            }
            if Instant::now() >= deadline {
                break Stopped::TimedOut;
            }
            std::thread::sleep(POLL);
        };

        // Whatever ended the wait, the hook must not outlive this call: a
        // timed-out hook still running would race the session it seeded. A
        // `ReadFinished` hook is still running, with its stdout closed and so
        // nothing more to read — it is killed for the same reason.
        if !matches!(stopped, Stopped::Exited(_)) {
            kill_group(&mut child);
            let _ = child.wait();
        }

        // A reader that failed or panicked is not the same as a hook that wrote
        // nothing: collapsing the two would establish an empty environment for a
        // launch and report success.
        let (raw, over) = match out.join() {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => {
                return Err(LaunchError::Read {
                    program: named.clone(),
                    detail: error.to_string(),
                });
            }
            Err(_) => {
                return Err(LaunchError::Read {
                    program: named.clone(),
                    detail: "the reader thread did not finish".to_string(),
                });
            }
        };
        if over {
            return Err(LaunchError::TooLarge {
                program: named.clone(),
                limit: limits.max_output,
            });
        }
        match stopped {
            Stopped::Exited(status) if status.success() => Ok(raw),
            Stopped::Exited(status) => Err(LaunchError::Exited {
                program: named.clone(),
                status,
            }),
            Stopped::Wait(detail) => Err(LaunchError::Wait {
                program: named.clone(),
                detail,
            }),
            Stopped::TimedOut => Err(LaunchError::TimedOut {
                program: named.clone(),
                limit: limits.timeout,
            }),
            Stopped::ReadFinished => Ok(raw),
        }
    })?;

    parse(&outcome, &program)
}

/// What ended the wait for a hook.
enum Stopped {
    Exited(ExitStatus),
    /// `try_wait` failed — the child cannot be collected, so it is killed and
    /// reported rather than left running.
    Wait(String),
    TimedOut,
    /// The stdout reader finished while the hook was still running, so its
    /// output is complete and nothing further can arrive.
    ReadFinished,
}

/// SIGKILL the hook's process group.
///
/// # Safety
///
/// `process_group(0)` above made the child the leader of a group whose id is its
/// pid, so this signal reaches this hook's processes only, and the child is
/// waited on only afterwards, so that pid is still live.
fn kill_group(child: &mut Child) {
    // SAFETY: a negative pid addresses the process group led by `child.id()`,
    // which the caller created for this hook alone.
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
}

/// Read at most `cap` bytes, reporting whether the pipe held more. Reading one
/// byte past the cap is what separates "exactly at the limit" from "over it",
/// which a read capped at `cap` cannot.
fn read_capped(pipe: &mut dyn Read, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    pipe.take(cap as u64 + 1).read_to_end(&mut buf)?;
    let over = buf.len() > cap;
    buf.truncate(cap);
    Ok((buf, over))
}

/// Parse a hook's stdout into an environment.
///
/// NUL-delimited and split at the first `=`, so a value keeps any `=` it
/// contains; empty records are skipped, which is what `env -0`'s trailing
/// delimiter produces. [`EnvSnapshot::new`] validates the result, so the only
/// checks here are the structure the delimiter scheme itself requires.
fn parse(raw: &[u8], program: &str) -> Result<EnvSnapshot, LaunchError> {
    let mut values = BTreeMap::new();
    for (index, record) in raw.split(|byte| *byte == b'\0').enumerate() {
        if record.is_empty() {
            continue;
        }
        let at = index + 1;
        let Some(split) = record.iter().position(|byte| *byte == b'=') else {
            return Err(LaunchError::RecordWithoutName {
                program: program.to_string(),
                record: at,
            });
        };
        let name =
            std::str::from_utf8(&record[..split]).map_err(|_| LaunchError::RecordNotUtf8 {
                program: program.to_string(),
                record: at,
                part: "name",
            })?;
        let value =
            std::str::from_utf8(&record[split + 1..]).map_err(|_| LaunchError::RecordNotUtf8 {
                program: program.to_string(),
                record: at,
                part: "value",
            })?;
        values.insert(name.to_string(), value.to_string());
    }
    EnvSnapshot::new(values).map_err(|detail| LaunchError::Environment {
        program: program.to_string(),
        detail,
    })
}

/// Why a launch environment could not be established.
///
/// Every variant names the configured program and, where there is one, a
/// position. None carries a variable's value or a record's contents: this text
/// reaches a log and an API response, and a hook's stdout is the payload most
/// likely to hold something secret.
#[derive(Debug)]
pub enum LaunchError {
    /// `env_hook` is present but names no program. Configuration validation
    /// rejects that spelling; this keeps the function from spawning an empty argv.
    NoProgram,
    /// The program could not be spawned at all — usually a name that is not on
    /// the host environment's `PATH`.
    Spawn { program: String, detail: String },
    /// The hook outlived its limit and its process group was killed.
    TimedOut { program: String, limit: Duration },
    /// The hook wrote more than its limit allows and was killed.
    TooLarge { program: String, limit: usize },
    /// The hook exited nonzero. Its stderr is deliberately not quoted here; see
    /// [`STDERR_LIMIT`].
    Exited { program: String, status: ExitStatus },
    /// The hook's exit could not be collected.
    Wait { program: String, detail: String },
    /// The hook's output could not be read to the end.
    Read { program: String, detail: String },
    /// A record carried no `=`, so it names no variable.
    RecordWithoutName { program: String, record: usize },
    /// A record's name or value was not valid UTF-8.
    RecordNotUtf8 {
        program: String,
        record: usize,
        part: &'static str,
    },
    /// The records are not an environment the existing constraints allow.
    Environment { program: String, detail: String },
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProgram => write!(f, "env_hook names no program to run"),
            Self::Spawn { program, detail } => {
                write!(f, "could not run env_hook program `{program}`: {detail}")
            }
            Self::TimedOut { program, limit } => write!(
                f,
                "env_hook `{program}` did not finish within {}s and was killed",
                limit.as_secs()
            ),
            Self::TooLarge { program, limit } => write!(
                f,
                "env_hook `{program}` wrote more than {limit} bytes of environment \
                 and was killed"
            ),
            Self::Exited { program, status } => match status.code() {
                Some(code) => write!(f, "env_hook `{program}` exited {code}"),
                None => write!(f, "env_hook `{program}` was killed by a signal"),
            },
            Self::Wait { program, detail } => {
                write!(f, "could not wait for env_hook `{program}`: {detail}")
            }
            Self::Read { program, detail } => {
                write!(f, "could not read env_hook `{program}` output: {detail}")
            }
            Self::RecordWithoutName { program, record } => write!(
                f,
                "env_hook `{program}` wrote record {record} of its environment \
                 without an `=`"
            ),
            Self::RecordNotUtf8 {
                program,
                record,
                part,
            } => write!(
                f,
                "env_hook `{program}` wrote a {part} in record {record} of its \
                 environment that is not valid UTF-8"
            ),
            Self::Environment { program, detail } => write!(
                f,
                "env_hook `{program}` wrote an environment the harness cannot use: \
                 {detail}"
            ),
        }
    }
}

impl std::error::Error for LaunchError {}

#[cfg(test)]
mod test;
