//! What the TUI says when its event stream ends (docs/tui.md).
//!
//! The stream ends for several reasons, and the screen the TUI restores shows none of
//! them. After the terminal is back, the entry point asks the host what became of the
//! session and prints one stderr line: the session was archived (and by whom, when the
//! `Archived` entry records it), the host stopped the session, or the host does not
//! answer.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::client::{Client, SessionRead};
use crate::protocol::sessions::ListedStatus;

/// What the host said about the session whose stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Closure {
    /// Archived on purpose; `by` is the `Archived` entry's `source`, when it has one.
    Archived {
        id: String,
        by: Option<String>,
        log: PathBuf,
    },
    /// On disk and not running, with no `Archived` entry last: the host stopped it.
    Stopped { id: String, log: PathBuf },
    /// The host still runs it, so the stream ended for another reason.
    StillHosted { id: String, status: ListedStatus },
    /// The host holds no such session and finds none on disk (a session that never had
    /// a turn is discarded rather than archived).
    Unknown { id: String },
    /// The host did not answer.
    HostGone { socket: Option<PathBuf> },
}

impl Closure {
    /// The line printed to stderr.
    pub(crate) fn line(&self) -> String {
        match self {
            Self::Archived { id, by, log } => {
                let by = by
                    .as_ref()
                    .map_or_else(String::new, |by| format!(" by {by}"));
                format!(
                    "efference: {id} was archived{by}; its log stays at {}",
                    shown(log)
                )
            }
            Self::Stopped { id, log } => format!(
                "efference: {id} stopped when the host did; its log stays at {}",
                shown(log)
            ),
            Self::StillHosted { id, status } => format!(
                "efference: the stream for {id} ended while the host still lists it as {status}"
            ),
            Self::Unknown { id } => format!("efference: {id} is no longer on the host"),
            Self::HostGone {
                socket: Some(socket),
            } => format!(
                "efference: lost the connection to the host at {}",
                socket.display()
            ),
            Self::HostGone { socket: None } => {
                "efference: lost the connection to the host".to_string()
            }
        }
    }
}

/// The session directory with a trailing separator, the way a directory is written.
fn shown(dir: &Path) -> String {
    format!("{}/", dir.display())
}

/// How long an answer that may still be changing is asked again. The stream ends when
/// the session's loop is shut down, and the archive finishes after that: the worktree
/// is retired, then `Archived` is appended and the session leaves the host's map.
const SETTLE: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);

/// Ask the host what became of the session `client` was attached to, once the stream
/// has closed. An answer that says the session is still hosted may be the middle of an
/// archive, so it is asked again for a bounded time before it is reported. One on disk
/// is final: a session leaves the host's map only once its `Archived` entry is written
/// (docs/control-api.md), so one without it was stopped.
pub(crate) async fn ask(client: &Client) -> Closure {
    ask_within(client, SETTLE).await
}

async fn ask_within(client: &Client, budget: Duration) -> Closure {
    let deadline = Instant::now() + budget;
    loop {
        let closure = ask_once(client).await;
        let changing = matches!(closure, Closure::StillHosted { .. });
        if !changing || Instant::now() >= deadline {
            return closure;
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn ask_once(client: &Client) -> Closure {
    let gone = || Closure::HostGone {
        socket: client.socket().map(Path::to_path_buf),
    };
    // A bind with no host above it names no session to ask about.
    let Some(id) = client.session_id().map(str::to_string) else {
        return gone();
    };
    match client.read_session(&id).await {
        Err(_) => gone(),
        Ok(SessionRead::Unknown) => Closure::Unknown { id },
        Ok(SessionRead::Hosted(row)) => Closure::StillHosted {
            id,
            status: row.status,
        },
        Ok(SessionRead::Dormant(dormant)) => {
            let row = dormant.archived.row;
            let log = row
                .cwd
                .join(crate::record::session_dir::SESSION_BASE)
                .join(&id);
            if row.interrupted {
                Closure::Stopped { id, log }
            } else {
                Closure::Archived {
                    id,
                    by: row.archived_by,
                    log,
                }
            }
        }
    }
}

#[cfg(test)]
mod test;
