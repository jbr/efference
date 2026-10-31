//! Serialized process spawning (docs/process-monitor.md).
//!
//! Every child this crate starts goes through one lock. `Command::spawn` creates
//! the child's stdio pipes and then starts the process; on platforms without an
//! atomic `pipe2(O_CLOEXEC)` (macOS among them) there is a window between creating
//! a pipe and marking it close-on-exec, and a spawn on another thread inside that
//! window hands its child a copy of the pipe's write end. That child, or anything
//! it forks, then keeps the first command's stdout open after the first command
//! has exited, and the reader waits for EOF until the stranger exits too. One
//! session's `sleep 30 &` was enough to stall another session's `echo`.
//!
//! Holding a lock across `spawn()` keeps pipe creation and process start of one
//! spawn from interleaving with another's. The critical section is a few system
//! calls; nothing awaits inside it.
//!
//! [`kill_group`] is the other end of a child spawned as the leader of its own
//! process group.

use std::io;
use std::sync::Mutex;

static SPAWN: Mutex<()> = Mutex::new(());

/// Spawn a `std` command under the process-wide spawn lock.
pub fn std(cmd: &mut std::process::Command) -> io::Result<std::process::Child> {
    let _serialized = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The one sanctioned spawn: the lock above is held (clippy.toml).
    #[allow(clippy::disallowed_methods)]
    cmd.spawn()
}

/// `Command::output` under the process-wide spawn lock: stdout and stderr piped
/// and collected, as `output` does. `output` spawns internally, so a call to it
/// is a spawn outside the lock.
pub fn std_output(cmd: &mut std::process::Command) -> io::Result<std::process::Output> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    std(cmd)?.wait_with_output()
}

/// Spawn a `tokio` command under the process-wide spawn lock. `tokio`'s `spawn`
/// is synchronous — it wraps the `std` spawn and registers the child afterward —
/// so the lock is held for the same few system calls.
pub fn tokio(cmd: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    let _serialized = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The one sanctioned spawn: the lock above is held (clippy.toml).
    #[allow(clippy::disallowed_methods)]
    cmd.spawn()
}

/// `SIGKILL` the whole process group led by `pgid`. The child was spawned with
/// `process_group(0)`, so its pid *is* its pgid; killing the group takes down
/// any grandchildren too. Best-effort — a racing exit may already have reaped
/// it, in which case `killpg` just returns `ESRCH`. Used by the foreground abort
/// path and by the process monitor's kill / teardown (docs/process-monitor.md).
pub(crate) fn kill_group(pgid: u32) {
    // SAFETY: an FFI call with no memory effects; a stale/invalid pgid is a
    // benign `ESRCH`, which we intentionally ignore (best-effort teardown).
    #[allow(unsafe_code)]
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
}

/// Test probe: whether group `pgid` still has a member. `killpg` with signal 0 checks
/// existence and permission only, so a killed process still answers until its parent
/// waits it (docs/process-monitor.md).
#[cfg(test)]
pub(crate) fn group_alive(pgid: u32) -> bool {
    // SAFETY: signal 0 checks existence and permission only; no memory effects.
    #[allow(unsafe_code)]
    unsafe {
        libc::killpg(pgid as libc::pid_t, 0) == 0
    }
}
