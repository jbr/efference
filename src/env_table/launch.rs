//! Establishing a session's launch environment at a directory (docs/env.md).
//!
//! The **launch environment** is the base a session's `[env]` policy is applied
//! to — what the operator's shell, `direnv`, or version manager had put in the
//! environment for one directory. Two sources, and the user-wide `env_hook`
//! chooses between them:
//!
//! - **No hook**: the host environment, as given. Nothing is spawned, and no
//!   shell startup is reconstructed. Whatever environment the host process was
//!   started with is the baseline. Whatever that is — a login shell's exports,
//!   or a service manager's environment — it is not a loaded project
//!   environment, and reconstructing one is the hook's own doing.
//! - **A hook**: an argv, invoked **directly** with the chosen directory set on
//!   the subprocess — never a shell `cd`, never a change to the host process's
//!   cwd. It inherits the host environment, so it can reach `mise`, `direnv`, or
//!   the user's own binaries, and reconstructing a richer environment is the
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
//!
//! ## Termination
//!
//! One deadline covers the whole operation — reading stdout, and collecting the
//! exit status — because a hook that closes its pipes has not thereby finished,
//! and waiting on it outside the deadline would leave a launch unbounded. Both
//! stdout reaching EOF and the child exiting are necessary and neither is
//! sufficient: a hook can close stdout early and then exit nonzero, which is why
//! success is only ever an *observed* successful exit.
//!
//! Cleanup is a [`ProcessGroup`] guard rather than a kill at the end of a happy
//! path: a caller that stops awaiting — a cancelled request, a shutting-down
//! runtime — drops this future, and the guard's `Drop` still signals the group.
//! Signalling and reaping are separate: `Drop` sends the kill, while collecting
//! the direct child is [`reap`]'s job on the paths that are still awaited. The
//! group is killed on every terminating path except a hook that exited
//! successfully with its pipes at EOF. A process group is not containment: a
//! descendant that puts itself in a new group is outside what one `killpg`
//! reaches.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::EnvSnapshot;

/// How long a hook may run before it is killed, covering the read and the exit
/// together. A hook is a startup script, so this allows for shell
/// initialization and a version manager's shims; it is finite because creation
/// is synchronous behind it. Unlike a reference resolution — which waits on a
/// human unlocking a vault, and so gets [`crate::env_table::RESOLVE_TIMEOUT`] —
/// a hook has no stdin and so has nothing to wait for.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of a hook's stdout is retained. A shell environment is kilobytes;
/// this is the bound chosen for a read that happens once per launch, so that a
/// runaway process has a ceiling here rather than whatever the host has spare.
/// Output past it ends the launch instead of being drained on.
pub const HOOK_OUTPUT_LIMIT: usize = 1024 * 1024;

/// How much is read per pipe read.
const CHUNK: usize = 8 * 1024;

/// The environment a session launched at `cwd` should carry: the hook's, or the
/// host's when no hook is configured.
///
/// `host_env` is the host's own environment, taken as an argument rather than
/// captured here, so the baseline a launch inherits is the caller's to name —
/// and so both branches can be driven without touching the process environment.
///
/// `abort` cancels the launch; it is a parameter rather than a signal handler
/// installed here so the kill path is reachable from a test. The deadline and
/// `abort` share one race, so whichever fires first ends the hook.
pub async fn environment_at(
    cwd: &Path,
    hook: Option<&[String]>,
    host_env: &EnvSnapshot,
    abort: impl Future<Output = ()> + Send,
) -> Result<EnvSnapshot, LaunchError> {
    run_hook(cwd, hook, host_env, abort, HookLimits::default()).await
}

/// The bounds one hook run is held to. The defaults are the documented limits; a
/// test lowers them so it need not wait 30 seconds or write a megabyte.
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

/// [`environment_at`] with the bounds spelled out — the interface that lets the
/// timeout and output-limit tests run in milliseconds.
async fn run_hook(
    cwd: &Path,
    hook: Option<&[String]>,
    host_env: &EnvSnapshot,
    abort: impl Future<Output = ()> + Send,
    limits: HookLimits,
) -> Result<EnvSnapshot, LaunchError> {
    if !cwd.is_absolute() {
        return Err(LaunchError::RelativeCwd {
            cwd: cwd.display().to_string(),
        });
    }
    let Some(argv) = hook else {
        return Ok(host_env.clone());
    };
    let (program, args) = argv.split_first().ok_or(LaunchError::NoProgram)?;
    let program = program.clone();

    let mut command = tokio::process::Command::new(&program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Nothing is kept from stderr, and nothing is reported: a startup script
        // may print a token it fetched. Null rather than piped, so there is no
        // reader to keep up and no pipe capacity for a chatty script to fill.
        .stderr(Stdio::null())
        // Its own group, so one killpg reaches the hook and whatever it spawned
        // (mirrors bash.rs). `kill_on_drop` covers the direct child when this
        // future is dropped without being awaited; the guard covers the group.
        .process_group(0)
        .kill_on_drop(true);
    // The baseline is replaced wholesale rather than inherited, so the hook sees
    // exactly what the host has and nothing of this process's own.
    host_env.apply_tokio(&mut command);

    let mut child =
        crate::platform::spawn::tokio(&mut command).map_err(|error| LaunchError::Spawn {
            program: program.clone(),
            detail: error.to_string(),
        })?;
    // `killpg` needs the pgid, and the child leads the group, so its pid is it.
    let pgid = child.id().ok_or_else(|| LaunchError::Spawn {
        program: program.clone(),
        detail: "the hook has no process id to clean up".to_string(),
    })?;
    let mut group = ProcessGroup::armed(pgid);
    let mut stdout = child.stdout.take().expect("stdout is piped");

    // The deadline is absolute and set once, so the time spent reading is not
    // spent again waiting for the exit.
    let deadline = tokio::time::Instant::now() + limits.timeout;

    let work = async {
        let raw = read_stdout(&mut stdout, limits.max_output, &program).await?;
        // Pipes at EOF do not mean the hook is done; it may have closed them and
        // kept running. Waiting here is inside the race below, not after it.
        let status = child.wait().await.map_err(|error| LaunchError::Wait {
            program: program.clone(),
            detail: error.to_string(),
        })?;
        Ok::<_, LaunchError>((raw, status))
    };

    tokio::pin!(abort);
    let halt = {
        let cancel = async {
            tokio::select! {
                _ = &mut abort => Cancelled::Aborted,
                _ = tokio::time::sleep_until(deadline) => Cancelled::TimedOut,
            }
        };
        tokio::pin!(cancel);
        tokio::select! {
            biased;
            reason = &mut cancel => Halt::Cancelled(reason),
            finished = work => Halt::Finished(finished),
        }
    };

    match halt {
        Halt::Cancelled(Cancelled::Aborted) => {
            group.kill();
            reap(&mut child).await;
            Err(LaunchError::Aborted { program })
        }
        Halt::Cancelled(Cancelled::TimedOut) => {
            group.kill();
            reap(&mut child).await;
            Err(LaunchError::TimedOut {
                program,
                limit: limits.timeout,
            })
        }
        // A read that overran, or a wait that failed: the launch is over, so the
        // group goes with it even though the caller might otherwise have let the
        // child finish.
        Halt::Finished(Err(error)) => {
            group.kill();
            reap(&mut child).await;
            Err(error)
        }
        Halt::Finished(Ok((_, status))) if !status.success() => {
            group.kill();
            reap(&mut child).await;
            Err(LaunchError::Exited { program, status })
        }
        // Success is an observed successful exit *and* output that parses, never
        // either alone. Parsing happens before the guard is disarmed: a zero-exit
        // hook that wrote something unusable is a failed launch, and the
        // descendants of a failed launch do not outlive it.
        Halt::Finished(Ok((raw, _))) => match parse(&raw, &program) {
            Ok(base) => {
                group.disarm();
                Ok(base)
            }
            Err(error) => {
                group.kill();
                reap(&mut child).await;
                Err(error)
            }
        },
    }
}

/// Collect the exit status, for the paths that kill first. The child has been
/// signalled, so this reaps rather than waits for more work.
async fn reap(child: &mut tokio::process::Child) {
    let _ = child.wait().await;
}

/// Why the launch was cut short.
enum Cancelled {
    Aborted,
    TimedOut,
}

/// What ended the wait: a cancellation source, or the hook finishing (which may
/// itself be a failure).
enum Halt {
    Cancelled(Cancelled),
    Finished(Result<(Vec<u8>, std::process::ExitStatus), LaunchError>),
}

/// Read the hook's stdout to EOF, keeping at most `cap` bytes. Output past the
/// cap is an error rather than a truncation: a truncated environment is not one
/// worth seeding a session with, and the caller ends the launch instead of
/// waiting out the deadline on a process that is still writing.
async fn read_stdout(
    stdout: &mut tokio::process::ChildStdout,
    cap: usize,
    program: &str,
) -> Result<Vec<u8>, LaunchError> {
    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let count = stdout
            .read(&mut buf)
            .await
            .map_err(|error| LaunchError::Read {
                program: program.to_string(),
                detail: error.to_string(),
            })?;
        if count == 0 {
            return Ok(raw);
        }
        if raw.len() + count > cap {
            return Err(LaunchError::TooLarge {
                program: program.to_string(),
                limit: cap,
            });
        }
        raw.extend_from_slice(&buf[..count]);
    }
}

/// The hook's process group, killed unless the launch succeeds.
///
/// Dropping this kills the group. That is what makes cancellation reach the
/// subprocess when the caller stops awaiting: a future that is dropped mid-flight
/// never runs the code that would kill at the end of a path, so the kill lives in
/// `Drop` instead. The direct child is killed with the group, and
/// `kill_on_drop` on the command covers the case where the guard is gone too.
struct ProcessGroup {
    pgid: Option<u32>,
}

impl ProcessGroup {
    fn armed(pgid: u32) -> Self {
        Self { pgid: Some(pgid) }
    }

    /// Stop managing the group, for a launch that succeeded.
    fn disarm(&mut self) {
        self.pgid = None;
    }

    fn kill(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            crate::platform::spawn::kill_group(pgid);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Parse a hook's stdout into an environment.
///
/// The contract is strict: a dump is NUL-delimited and **NUL-terminated**, so a
/// final record with no trailing delimiter is refused — that is what a truncated
/// write looks like, and accepting it would seed a session with a partial
/// environment. Zero bytes with a successful exit is a valid empty environment,
/// which is how a deliberately cleared baseline reports itself. Empty records
/// between delimiters are skipped, which is what `env -0`'s trailing delimiter
/// produces. [`EnvSnapshot::new`] validates the result, so the only checks here
/// are the structure the delimiter scheme itself requires.
fn parse(raw: &[u8], program: &str) -> Result<EnvSnapshot, LaunchError> {
    if raw.is_empty() {
        return EnvSnapshot::new(BTreeMap::new()).map_err(|detail| LaunchError::Environment {
            program: program.to_string(),
            detail,
        });
    }
    if !raw.ends_with(b"\0") {
        return Err(LaunchError::Unterminated {
            program: program.to_string(),
        });
    }
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
    /// The directory a launch was asked for is relative, so a hook would run in
    /// the host's own working directory instead.
    RelativeCwd { cwd: String },
    /// `env_hook` is present but names no program. Configuration validation
    /// rejects that spelling; this keeps the function from spawning an empty argv.
    NoProgram,
    /// The program could not be spawned at all — usually a name that is not on
    /// the host environment's `PATH`.
    Spawn { program: String, detail: String },
    /// The caller cancelled the launch.
    Aborted { program: String },
    /// The hook outlived its limit and its process group was killed.
    TimedOut { program: String, limit: Duration },
    /// The hook wrote more than its limit allows; its group was killed.
    TooLarge { program: String, limit: usize },
    /// The hook exited nonzero.
    Exited {
        program: String,
        status: std::process::ExitStatus,
    },
    /// The hook's exit could not be collected.
    Wait { program: String, detail: String },
    /// The hook's output could not be read to the end.
    Read { program: String, detail: String },
    /// The output was not NUL-terminated, so the last record may be truncated.
    Unterminated { program: String },
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
            Self::RelativeCwd { cwd } => write!(
                f,
                "a launch environment is established at an absolute directory, not \
                 {cwd:?}"
            ),
            Self::NoProgram => write!(f, "env_hook names no program to run"),
            Self::Spawn { program, detail } => {
                write!(f, "could not run env_hook program `{program}`: {detail}")
            }
            Self::Aborted { program } => {
                write!(f, "env_hook `{program}` was cancelled and its group killed")
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
            Self::Unterminated { program } => write!(
                f,
                "env_hook `{program}` wrote an environment whose last record is not \
                 NUL-terminated, so it may be truncated"
            ),
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
