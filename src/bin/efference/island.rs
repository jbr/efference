//! No host running: start a terminal-owned island, create a session here, and
//! attach (docs/operating.md).
//!
//! The host is a separate process from birth (`efference host --island`): its own
//! OS session, so the terminal's Ctrl-C and hangup never reach it; stdout and
//! stderr to a log file, so nothing it prints lands over the full-screen UI and a
//! host that outlives its launcher never writes into a closed pipe; and stdin a
//! pipe this process holds, which is the ownership connection
//! ([`efference::daemon::island`]). Until the TUI takes the screen, the log is
//! echoed here — that is the boot report the creating terminal would otherwise
//! not see.

use super::*;
use efference::daemon::island::{ISLAND_LOG, ISLAND_SOCKET, island_dir};
use std::io::Read;
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

    /// Wait until the socket answers, echoing the host's log. A host that exits
    /// first has said why in the log, which has been echoed by then.
    pub(crate) async fn wait_ready(&mut self, tail: &mut LogTail) -> Result<(), CliError> {
        loop {
            tail.echo();
            if is_live(&self.socket) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                tail.echo();
                return Err(format!("the host exited during startup ({status})").into());
            }
            tokio::time::sleep(POLL).await;
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

/// The host's log, read forward from where the last read stopped and echoed a
/// complete line at a time.
pub(crate) struct LogTail {
    file: std::fs::File,
    partial: Vec<u8>,
}

impl LogTail {
    pub(crate) fn open(island: &Island) -> Result<Self, CliError> {
        Ok(Self {
            file: std::fs::File::open(&island.log)?,
            partial: Vec::new(),
        })
    }

    /// Echo every complete line written since the last call.
    pub(crate) fn echo(&mut self) {
        let _ = self.file.read_to_end(&mut self.partial);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return;
        };
        let lines: Vec<u8> = self.partial.drain(..=end).collect();
        for line in String::from_utf8_lossy(&lines).lines() {
            eprintln!("  {line}");
        }
    }

    /// Echo on a timer until the returned stop is sent, then once more, and hand
    /// the tail back.
    fn echo_in_background(
        mut self,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Self>,
    ) {
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            loop {
                self.echo();
                tokio::select! {
                    _ = &mut stopped => {
                        self.echo();
                        return self;
                    }
                    _ = tokio::time::sleep(POLL) => {}
                }
            }
        });
        (stop, task)
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
    eprintln!("no efference host is running; starting one for this terminal…");
    let mut island = Island::launch()?;
    let mut tail = LogTail::open(&island)?;
    if let Err(error) = island.wait_ready(&mut tail).await {
        island.end().await;
        return Err(error);
    }
    let (stop, echo) = tail.echo_in_background();
    let created = create_here(island.socket(), name, flags).await;
    let _ = stop.send(());
    let _ = echo.await;
    let id = match created {
        Ok(id) => id,
        Err(error) => {
            island.end().await;
            return Err(error);
        }
    };
    let attached = attach_created(island.socket(), &id, theme, images, frame_cost, true).await;
    island.end().await;
    attached
}
