//! The HTTP-over-control-channel executor the command line and `efference mcp` use
//! (docs/tools.md): which session a call reaches, the reads every verb shares, and the
//! agents an MCP mount watches for `wait` and streams (docs/operating.md).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::stream::Streams;
use super::watch::Watch;
use crate::client::{Client, Waited};
use crate::protocol::status::StatusView;

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
    /// The agents this mount watches and what it has shown of each, for `wait`
    /// (docs/operating.md). Shared by every clone, so concurrent calls see one set.
    watch: Arc<Watch>,
    /// This mount's status streams (docs/operating.md). Shared by every clone, so one
    /// server and one token per agent serve every call.
    streams: Arc<Streams>,
}

/// Which session a client executor drives when a call names no `agent`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum Fallback {
    /// One already reached: a test's in-memory transport.
    Client(Client),
    /// The one session the host lists in this directory, found on first use and
    /// then kept. The command line's default, so `efference control state` in a
    /// directory with one session needs no `--agent` and a host-only verb never
    /// triggers discovery.
    Discover,
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
            watch: Arc::default(),
            streams: Arc::default(),
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

    /// An executor over `host` whose default session is `id` on it, for the verbs
    /// that may name no agent.
    #[cfg(test)]
    pub(crate) fn defaulting_to(host: Client, id: &str) -> Self {
        let session = Client::clone(&host).with_prefix(&format!("/sessions/{id}"));
        Self {
            fallback: Arc::new(Mutex::new(Fallback::Client(session))),
            ..Self::over(host)
        }
    }

    /// Whether a call that names no agent reads the host as a whole rather than one
    /// session: nothing names a session for this mount.
    pub(super) fn host_wide(&self) -> bool {
        !matches!(
            &*self
                .fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Fallback::Client(_)
        )
    }

    /// The client for one call: the `agent` id if given — a session the host
    /// lists — else the fallback. Without a fallback, a call that names no agent
    /// is an error that says what the host holds.
    pub(super) async fn client(&self, agent: Option<&str>) -> Result<Client> {
        if let Some(name) = agent {
            if let Some(host) = &self.host_client {
                return Ok(Client::clone(host).with_prefix(&format!("/sessions/{name}")));
            }
            return named(self.host_client().ok(), &self.host, name).await;
        }
        // Cloned out rather than held: resolving awaits, and the lock is sync.
        // Two calls racing to resolve both reach the same answer.
        let fallback = self
            .fallback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match fallback {
            Fallback::Client(client) => Ok(client),
            Fallback::Discover => {
                let client = self.discover().await?;
                *self
                    .fallback
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
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

    /// The one session the host lists in this directory. The listing is scoped to
    /// it: one host serves many projects, and a verb run here means a session here
    /// (docs/control-channel.md). A listing without a cwd is kept.
    async fn discover(&self) -> Result<Client> {
        let mut hosted: Vec<String> = if Self::is_live(&self.host) {
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
        match hosted.len() {
            0 => Err(anyhow!(
                "no live efference session found here — create one with `efference control create`, or pass --agent"
            )),
            1 => Ok(Client::at(
                &self.host,
                &format!("/sessions/{}", hosted.pop().unwrap()),
            )),
            _ => Err(anyhow!(
                "multiple live sessions ({}); pass --agent to choose",
                hosted.join(", ")
            )),
        }
    }

    fn is_live(socket: &Path) -> bool {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
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
                "no host at {}; start one with `efference --host`",
                self.host.display()
            ));
        }
        Ok(Client::new(&self.host))
    }

    /// The last seq this mount was shown of `id`'s transcript.
    pub(super) fn cursor(&self, id: &str) -> Option<u64> {
        self.cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
    }

    /// Record the last seq this mount was shown of `id`'s transcript.
    pub(super) fn set_cursor(&self, id: &str, seq: u64) {
        self.cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string(), seq);
    }

    /// Watch `id` for `wait` from now on (docs/operating.md). `shown` is the status this
    /// mount just showed of it — a creation's — or `None` when it has shown none.
    pub(crate) fn watch(&self, id: &str, shown: Option<&StatusView>) -> Result<()> {
        self.watch.add(&self.host_client()?, id, shown);
        Ok(())
    }

    /// Watch `id`, which a `wait` named: an error naming what the host holds when the
    /// host does not hold it.
    pub(super) async fn watch_named(&self, id: &str) -> Result<()> {
        if self.watch.contains(id) {
            return Ok(());
        }
        let host = self.host_client()?;
        let ids = host.list_sessions().await?;
        if !ids.iter().any(|held| held == id) {
            return Err(match ids.is_empty() {
                true => anyhow!("no agent {id:?} on this host, which holds none; `create` one"),
                false => anyhow!("no agent {id:?} on this host; it holds {}", ids.join(", ")),
            });
        }
        self.watch.add(&host, id, None);
        Ok(())
    }

    /// The watched set (docs/operating.md).
    pub(super) fn watched(&self) -> &Watch {
        &self.watch
    }

    /// The URL of `agent`'s status stream on this mount (docs/operating.md); the first
    /// binds the stream server. `reader` is the client a frame names "you".
    pub(super) async fn stream_url(&self, agent: &str, reader: Option<&str>) -> Result<String> {
        self.streams.url(&self.host_client()?, agent, reader).await
    }

    /// A client for the hosted session `id`, through the host's socket.
    pub(super) fn hosted(&self, id: &str) -> Result<Client> {
        Ok(self.host_client()?.with_prefix(&format!("/sessions/{id}")))
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

/// The session `name`, resolved through `host` — a client at the host's socket root,
/// `None` when none is running — wherever this process runs (docs/control-channel.md).
/// `host_socket` is the host's path, for the error.
pub(crate) async fn named(host: Option<Client>, host_socket: &Path, name: &str) -> Result<Client> {
    let mut listed = None;
    if let Some(host) = &host {
        match host.list_sessions().await {
            Ok(ids) if ids.iter().any(|id| id == name) => {
                return Ok(host.clone().with_prefix(&format!("/sessions/{name}")));
            }
            Ok(_) => {}
            Err(e) => listed = Some(format!("{e:#}")),
        }
    }
    let host_says = match (host, listed) {
        (None, _) => format!("no host is running at {}", host_socket.display()),
        (Some(_), Some(why)) => format!(
            "the host at {} could not be listed: {why}",
            host_socket.display()
        ),
        (Some(_), None) => format!("the host at {} does not list it", host_socket.display()),
    };
    Err(anyhow!("no live session {name:?}: {host_says}"))
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
