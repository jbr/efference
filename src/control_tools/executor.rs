//! The HTTP-over-control-channel executor the command line and `efference mcp` use
//! (docs/tools.md): which session a call reaches, and the reads every verb shares.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::client::{Client, Waited};
use crate::daemon::status::StatusView;

/// The HTTP-over-control-channel executor used by the CLI and MCP mounts. An operator
/// client: it drives every session the host holds (docs/tools.md).
#[derive(Clone)]
pub struct ClientExecutor {
    host: PathBuf,
    /// A host reached over something other than its socket: a test's in-memory
    /// transport. `None` dials `host`.
    host_client: Option<Arc<Client>>,
    /// Shared by every clone, so a fallback resolved by one call is kept for the
    /// rest.
    fallback: Arc<Mutex<Fallback>>,
    /// The last seq this mount was shown of each session's transcript, by id
    /// (docs/tools.md). Shared by every clone, like the fallback; never logged.
    cursors: Arc<Mutex<HashMap<String, u64>>>,
}

/// Which session a client executor drives when a call names no `agent`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum Fallback {
    /// One already reached: a resolved `--name`/`--socket`, or a test's in-memory
    /// transport.
    Client(Client),
    /// Resolve from `--name`/`--socket`/discovery on first use, then keep it. The
    /// command line's default, so `efference control state` in a directory with
    /// one session needs no name and a host-only verb never triggers discovery.
    Resolve {
        name: Option<String>,
        socket: Option<PathBuf>,
    },
    /// Nothing: the server was mounted against the host, and every call names its
    /// agent. `efference mcp`'s default.
    None,
}

/// Whether two paths name the same directory: by canonical form when both
/// resolve, else as spelled (a session's identity carries the cwd its creator
/// sent, which is `current_dir` here when it is this directory).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

impl ClientExecutor {
    pub(super) fn new(host: PathBuf, fallback: Fallback) -> Self {
        Self {
            host,
            host_client: None,
            fallback: Arc::new(Mutex::new(fallback)),
            cursors: Arc::default(),
        }
    }

    /// An executor whose host is `host`, reached over whatever transport that client
    /// has — the interface a test drives a real host router through in memory. Every
    /// call names its agent, as on `efference mcp`.
    #[cfg(test)]
    pub(crate) fn over(host: Client) -> Self {
        Self {
            host_client: Some(Arc::new(host)),
            ..Self::new(PathBuf::from(".efference/host.sock"), Fallback::None)
        }
    }

    /// An executor whose default session is `session`, with no host behind it.
    #[cfg(test)]
    pub(crate) fn defaulting_to(session: Client) -> Self {
        Self::new(
            PathBuf::from(".efference/host.sock"),
            Fallback::Client(session),
        )
    }

    /// Whether a call that names no agent reads the host as a whole rather than one
    /// session: nothing names a session for this mount.
    pub(super) fn host_wide(&self) -> bool {
        matches!(
            &*self.fallback.lock().unwrap_or_else(|e| e.into_inner()),
            Fallback::None
                | Fallback::Resolve {
                    name: None,
                    socket: None
                }
        )
    }

    /// The client for one call: the `agent` id if given — a session the host
    /// lists, or one with its own live socket under `.efference/` — else the
    /// fallback. Without a fallback, a call that names no agent is an error that
    /// says what the host holds.
    pub(super) async fn client(&self, agent: Option<&str>) -> Result<Client> {
        if let Some(name) = agent {
            if let Some(host) = &self.host_client {
                return Ok(Client::clone(host).with_prefix(&format!("/sessions/{name}")));
            }
            return self.resolve(Some(name), None).await;
        }
        // Cloned out rather than held: resolving awaits, and the lock is sync.
        // Two calls racing to resolve both reach the same answer.
        let fallback = self
            .fallback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match fallback {
            Fallback::Client(client) => Ok(client),
            Fallback::Resolve { name, socket } => {
                let client = self.resolve(name.as_deref(), socket).await?;
                *self.fallback.lock().unwrap_or_else(|e| e.into_inner()) =
                    Fallback::Client(client.clone());
                Ok(client)
            }
            Fallback::None => {
                let held = match self.host_client() {
                    Ok(host) => match host.list_sessions().await {
                        Ok(ids) if ids.is_empty() => {
                            "the host holds no sessions; `create` one".to_string()
                        }
                        Ok(ids) => format!("the host holds: {}", ids.join(", ")),
                        Err(e) => format!("the host could not be listed: {e:#}"),
                    },
                    Err(e) => e.to_string(),
                };
                Err(anyhow!(
                    "pass `agent`: this server is mounted against the host, not one session ({held})"
                ))
            }
        }
    }

    async fn resolve(&self, name: Option<&str>, socket: Option<PathBuf>) -> Result<Client> {
        if let Some(socket) = socket {
            return Ok(Client::new(socket));
        }
        // The host's listing, scoped to this directory: one host serves many
        // projects, and a verb run here means a session here
        // (docs/control-channel.md). A listing without a cwd is kept.
        let hosted: Vec<String> = if Self::is_live(&self.host) {
            let here = std::env::current_dir()?;
            Client::new(&self.host)
                .sessions()
                .await?
                .into_iter()
                .filter(|listing| {
                    listing
                        .cwd
                        .as_deref()
                        .is_none_or(|cwd| same_dir(Path::new(cwd), &here))
                })
                .map(|listing| listing.id)
                .collect()
        } else {
            Vec::new()
        };
        if let Some(name) = name {
            let own = PathBuf::from(crate::daemon::SESSION_BASE)
                .join(name)
                .join(crate::daemon::SOCKET_NAME);
            if Self::is_live(&own) {
                return Ok(Client::new(own));
            }
            if hosted.iter().any(|id| id == name) {
                return Ok(Client::at(&self.host, &format!("/sessions/{name}")));
            }
            return Err(anyhow!(
                "no live session {name:?}: nothing listens at {} and the host{} does not list it",
                own.display(),
                if Self::is_live(&self.host) {
                    String::new()
                } else {
                    format!(" at {} is not running", self.host.display())
                }
            ));
        }
        let mut live: Vec<(String, Client)> = Self::discover_live_sockets()?
            .into_iter()
            .map(|socket| (socket.display().to_string(), Client::new(socket)))
            .collect();
        live.extend(hosted.iter().map(|id| {
            (
                format!("{} (/sessions/{id})", self.host.display()),
                Client::at(&self.host, &format!("/sessions/{id}")),
            )
        }));
        match live.len() {
            0 => Err(anyhow!(
                "no live efference session found — create one with `tui --new`, or pass --name/--socket"
            )),
            1 => Ok(live.pop().unwrap().1),
            _ => Err(anyhow!(
                "multiple live sessions ({}); pass --name to choose",
                live.into_iter()
                    .map(|(description, _)| description)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn is_live(socket: &Path) -> bool {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }

    fn discover_live_sockets() -> Result<Vec<PathBuf>> {
        let mut sockets = Vec::new();
        let base = Path::new(crate::daemon::SESSION_BASE);
        if base.is_dir() {
            for entry in std::fs::read_dir(base)? {
                let socket = entry?.path().join(crate::daemon::SOCKET_NAME);
                if Self::is_live(&socket) {
                    sockets.push(socket);
                }
            }
        }
        sockets.sort();
        Ok(sockets)
    }

    /// A client at the host's socket root, for the listener's own routes. Checked
    /// live first, so a host that is not running reads as that rather than as a
    /// connection error inside a request.
    pub(super) fn host_client(&self) -> Result<Client> {
        if let Some(host) = &self.host_client {
            return Ok(Client::clone(host));
        }
        if !Self::is_live(&self.host) {
            return Err(anyhow!(
                "no host at {}; start one with `efference host`",
                self.host.display()
            ));
        }
        Ok(Client::new(&self.host))
    }

    /// The last seq this mount was shown of `id`'s transcript.
    pub(super) fn cursor(&self, id: &str) -> Option<u64> {
        self.cursors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .copied()
    }

    /// Record the last seq this mount was shown of `id`'s transcript.
    pub(super) fn set_cursor(&self, id: &str, seq: u64) {
        self.cursors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), seq);
    }

    /// Run a waiting call and answer with its `/state` and whether it settled, bounded
    /// by `timeout_s` measured from the start of the call. `first` is the acting request
    /// (a bare settle, or a creation's wait). The host caps each wait and says so
    /// (docs/control-api.md), so a capped answer is followed by fresh settle polls until
    /// one is settled or the deadline passes; an omitted `timeout_s` has no deadline
    /// (docs/operating.md).
    pub(super) async fn settle_within<F>(
        &self,
        client: &Client,
        timeout_s: Option<u64>,
        first: F,
    ) -> Result<Waited>
    where
        F: Future<Output = Result<Waited>>,
    {
        let wait = async {
            let mut answer = first.await?;
            while !answer.settled {
                answer = client.settle().await?;
            }
            Ok(answer)
        };
        let Some(seconds) = timeout_s else {
            return wait.await;
        };
        match tokio::time::timeout(Duration::from_secs(seconds), wait).await {
            Ok(result) => result,
            Err(_) => Ok(Waited {
                body: client.state().await?,
                settled: false,
            }),
        }
    }
}

/// A `/state` body's status view (docs/control-api.md).
pub(super) fn status_in(state: &Value) -> Result<StatusView> {
    serde_json::from_value(state.get("status").cloned().unwrap_or(Value::Null))
        .map_err(|e| anyhow!("the host's state carries no readable status: {e}"))
}

/// `client`'s session's `/state`, parsed.
pub(super) async fn state_of(client: &Client) -> Result<Value> {
    Ok(serde_json::from_str(&client.state().await?)?)
}
