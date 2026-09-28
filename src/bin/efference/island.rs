//! No host running: start a terminal-owned island, create a session here, and
//! attach (docs/operating.md).
//!
//! The host is a separate process from birth (`efference host --island`): its own
//! OS session, so the terminal's Ctrl-C and hangup never reach it; stdout and
//! stderr to a log file, so nothing it prints lands over the full-screen UI and a
//! host that outlives its launcher never writes into a closed pipe; and stdin a
//! pipe this process holds, which is the ownership connection
//! ([`efference::daemon::island`]).
//!
//! An island leaves the terminal as it found it. The host's boot report stays in
//! the log: echoed before the TUI took the screen, it was left behind on the
//! normal screen when the TUI gave it back. The log is shown here only when
//! startup fails — the host exits, or the session cannot be created — which is
//! when its contents are the explanation.

use super::*;
use efference::daemon::island::{ISLAND_LOG, ISLAND_SOCKET, island_dir};
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::Duration;

/// How often the launcher looks at the log and the socket while it waits.
const POLL: Duration = Duration::from_millis(50);

/// A running island host and this process's end of its ownership pipe.
pub(crate) struct Island {
    child: std::process::Child,
    /// Held, never written. Dropping it — or this process ending any way at
    /// all — is what ends the host.
    owner: Option<std::process::ChildStdin>,
    socket: PathBuf,
    log: PathBuf,
}

impl Island {
    /// Start the host process. It is not ready until [`wait_ready`](Self::wait_ready).
    pub(crate) fn launch() -> Result<Self, CliError> {
        let home = std::env::var_os("HOME").ok_or("no HOME to place ~/.efference/islands under")?;
        let dir = island_dir(Path::new(&home), std::process::id());
        std::fs::create_dir_all(&dir)?;
        let socket = dir.join(ISLAND_SOCKET);
        let log = dir.join(ISLAND_LOG);
        let log_file = std::fs::File::create(&log)?;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .arg("--socket")
            .arg(&socket)
            .args(["host", "--island"])
            .stdin(Stdio::piped())
            .stdout(log_file.try_clone()?)
            .stderr(log_file);
        // SAFETY: `setsid` is async-signal-safe, and the closure allocates nothing.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = efference::spawn::std(&mut command)?;
        let owner = child.stdin.take();
        Ok(Self {
            child,
            owner,
            socket,
            log,
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    /// Wait until the socket answers. A host that exits first has said why in
    /// its log, which the caller shows.
    pub(crate) async fn wait_ready(&mut self) -> Result<(), CliError> {
        loop {
            if is_live(&self.socket) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(format!("the host exited during startup ({status})").into());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Print the host's log so far — the explanation for a failed startup.
    fn show_log(&self) {
        let Ok(log) = std::fs::read_to_string(&self.log) else {
            return;
        };
        eprintln!("the host's log:");
        for line in log.lines() {
            eprintln!("  {line}");
        }
    }

    /// Close the ownership pipe and wait for the host to finish tearing down its
    /// sessions. A host that fails leaves its log in place and says where.
    pub(crate) async fn end(mut self) {
        drop(self.owner.take());
        let log = self.log.clone();
        let mut child = self.child;
        let mut waiting = tokio::task::spawn_blocking(move || child.wait());
        // Said only when teardown is slow enough to look like a hang; the TUI may
        // have left the cursor mid-line, hence the leading newline.
        let status = match tokio::time::timeout(Duration::from_secs(1), &mut waiting).await {
            Ok(status) => status,
            Err(_) => {
                eprintln!("\nending this terminal's host and its sessions…");
                waiting.await
            }
        };
        match status {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => eprintln!(
                "the host exited with {status}; its log is {}",
                log.display()
            ),
            _ => eprintln!("could not wait for the host; its log is {}", log.display()),
        }
    }
}

/// The whole path: start the island, create a session in this directory, attach,
/// and end the island when the TUI exits.
pub(crate) async fn run(
    name: Option<String>,
    flags: SessionFlags,
    theme: Option<String>,
    images: efference::tui::ImageMode,
    frame_cost: bool,
) -> CliResult {
    // One status line while the host starts, erased before the TUI takes the
    // screen so nothing of the launch is left behind when it gives it back. Not
    // drawn when stderr is not a terminal, where there is no line to erase.
    let status = std::io::stderr().is_terminal();
    if status {
        eprint!("starting efference for this terminal…");
    }
    let end_status = |erase: bool| match (status, erase) {
        (true, true) => eprint!("\r\x1b[2K"),
        (true, false) => eprintln!(),
        (false, _) => {}
    };
    let mut island = Island::launch().inspect_err(|_| end_status(false))?;
    let created = match island.wait_ready().await {
        Ok(()) => create_here(island.socket(), name, flags).await,
        Err(error) => Err(error),
    };
    let id = match created {
        Ok(id) => id,
        Err(error) => {
            end_status(false);
            island.show_log();
            island.end().await;
            return Err(error);
        }
    };
    end_status(true);
    let attached = attach_created(island.socket(), &id, theme, images, frame_cost, true).await;
    island.end().await;
    attached
}
