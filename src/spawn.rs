//! Serialized process spawning (docs/supervisor.md).
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

use std::io;
use std::sync::Mutex;

static SPAWN: Mutex<()> = Mutex::new(());

/// Spawn a `std` command under the process-wide spawn lock.
pub fn std(cmd: &mut std::process::Command) -> io::Result<std::process::Child> {
    let _serialized = SPAWN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}

/// Spawn a `tokio` command under the process-wide spawn lock. `tokio`'s `spawn`
/// is synchronous — it wraps the `std` spawn and registers the child afterward —
/// so the lock is held for the same few system calls.
pub fn tokio(cmd: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    let _serialized = SPAWN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}
