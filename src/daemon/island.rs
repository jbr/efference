//! A terminal-owned host: its lifetime is bound to the client that launched it
//! (docs/control-channel.md).
//!
//! The launcher runs the host as a child whose stdin is a pipe it holds and never
//! writes. That pipe is the ownership connection: the kernel closes the launcher's
//! end however the launcher ends — a clean quit, a crash, SIGKILL — and the host
//! reads EOF. Nothing else carries ownership, so a second client attaching, a
//! stream reconnecting, or an agent driving the host over its socket cannot keep
//! the island alive or end it.
//!
//! Owner loss is watched from before the host has initialized anything. Until the
//! host exists there are no sessions to tear down and the process exits outright;
//! after, the host archives every session it holds, as `DELETE` would, and then
//! shuts down as a signal would, which reaps every session's tool processes before
//! the listener completes (docs/resumption.md, docs/process-monitor.md).

use super::Host;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

/// Where standalone hosts live: `~/.efference/standalone/<launcher pid>/`, one directory each.
pub const STANDALONE_DIR: &str = "standalone";

/// The island's control socket, inside its directory.
pub const ISLAND_SOCKET: &str = "host.sock";

/// The host's stdout and stderr, inside its directory. A file rather than a pipe
/// to the launcher, so a host that outlives its launcher never writes into a
/// closed pipe, and the boot report survives a crash for reading.
pub const ISLAND_LOG: &str = "host.log";

/// The directory for an island launched by process `pid`, under `home`.
pub fn island_dir(home: &Path, pid: u32) -> PathBuf {
    home.join(crate::session_dir::SESSION_BASE)
        .join(STANDALONE_DIR)
        .join(pid.to_string())
}

/// What owner loss acts on: the host once it exists, and whether the owner went
/// before it did.
#[derive(Default)]
struct Watch {
    host: Option<Adopted>,
    owner_gone: bool,
}

/// The adopted host, and the runtime its exit runs on: the watch thread is not one
/// of the runtime's. Held weakly, since the host's owner is the process that serves
/// it; a host already gone has nothing left to end.
#[derive(Clone)]
struct Adopted {
    host: Weak<Host>,
    runtime: tokio::runtime::Handle,
}

impl Adopted {
    /// Archive the host's sessions and shut it down, on the host's runtime
    /// ([`Host::archive_and_shut_down`]).
    fn exit(&self) {
        if let Some(host) = self.host.upgrade() {
            self.runtime
                .spawn(async move { host.archive_and_shut_down().await });
        }
    }
}

/// The host side of the ownership connection. Created before the host
/// initializes; [`adopt`](Self::adopt) hands it the host once there is one.
#[derive(Clone, Default)]
pub struct OwnerWatch(Arc<Mutex<Watch>>);

impl OwnerWatch {
    /// Read `owner` until it closes, on a thread of its own, then end the host:
    /// `before_host` runs if no host has been adopted yet (the process has nothing
    /// to wind down), otherwise the adopted one archives its sessions and shuts
    /// down.
    ///
    /// Bytes the owner writes are discarded; the pipe carries no protocol, only
    /// its own closing.
    pub fn spawn(
        owner: impl Read + Send + 'static,
        before_host: impl FnOnce() + Send + 'static,
    ) -> Self {
        let watch = Self::default();
        let shared = watch.clone();
        std::thread::spawn(move || {
            drain_until_closed(owner);
            shared.owner_lost(before_host);
        });
        watch
    }

    /// The host now exists, and is an island from here: it runs no boot resume, so
    /// this comes before the resume is spawned (docs/resumption.md). Called on the
    /// host's runtime, which its exit will run on.
    ///
    /// If the owner already went, the exit starts here: the watch thread saw no
    /// host and ran `before_host`, which for the process launch exits — so this
    /// branch is reached only by a `before_host` that returns.
    pub fn adopt(&self, host: &Arc<Host>) {
        host.become_island();
        let adopted = Adopted {
            host: Arc::downgrade(host),
            runtime: tokio::runtime::Handle::current(),
        };
        let mut watch = self.lock();
        if watch.owner_gone {
            adopted.exit();
        }
        watch.host = Some(adopted);
    }

    fn owner_lost(&self, before_host: impl FnOnce()) {
        let adopted = {
            let mut watch = self.lock();
            watch.owner_gone = true;
            watch.host.clone()
        };
        match adopted {
            Some(adopted) => {
                log::info!("island owner closed; archiving its sessions and shutting down");
                adopted.exit();
            }
            None => before_host(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Watch> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Read and discard until EOF or a read error; either is the owner's end closing.
fn drain_until_closed(mut owner: impl Read) {
    let mut sink = [0u8; 256];
    loop {
        match owner.read(&mut sink) {
            Ok(0) => return,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

/// Remove what an island leaves in its directory, then the directory if that
/// emptied it. Named files only, never a recursive removal: the directory is
/// derived from the socket path, which is a flag.
pub fn remove_island_files(socket: &Path) {
    let _ = std::fs::remove_file(socket);
    if let Some(dir) = socket.parent() {
        let _ = std::fs::remove_file(dir.join(ISLAND_LOG));
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod test;
