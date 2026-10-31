//! The clap command tree, session-directory layout, and socket resolution.

use super::*;

#[derive(Parser)]
#[command(
    name = "efference",
    version,
    about = "An API-backed agentic coding harness."
)]
pub(crate) struct Cli {
    /// Listen on a TCP address as well: the web client's front door. A port
    /// (`8080`), an address (`127.0.0.1:8000`, `0` for one the kernel picks), or
    /// `host:port`; a bare port means loopback, and a cleartext bind is
    /// accepted only there, so the short spelling never widens exposure.
    /// With `--host`, overrides `[host] bind`;
    /// `--bind ""` serves the Unix socket alone whatever the table says.
    ///
    /// Without `--host` this is the standalone host's: with no host running,
    /// a standalone host answers in the browser on that address instead of
    /// in the TUI.
    // docs/web-bind.md
    #[arg(long, global = true, value_name = "ADDR:PORT")]
    pub(crate) bind: Option<String>,
    /// The session to attach to, by id, without asking.
    #[arg(long, value_name = "ID", conflicts_with = "host")]
    pub(crate) name: Option<String>,
    #[command(flatten)]
    pub(crate) host: HostFlags,
    /// Omitted, `efference` runs `setup` when there is no host configuration
    /// yet, and otherwise opens the TUI: pick a session here, or with no host
    /// running, start one owned by this terminal. `--host` runs a host instead.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

impl Cli {
    /// `--host` and `--name` choose what bare `efference` does, so neither is
    /// accepted beside a subcommand, where it would be dropped. `--bind` is
    /// global and reaches `setup` too, which is why this is not clap's
    /// `args_conflicts_with_subcommands`.
    pub(crate) fn checked(self) -> Result<Self, clap::Error> {
        let flag = match (self.host.run, &self.name) {
            (true, _) => "--host",
            (false, Some(_)) => "--name",
            (false, None) => return Ok(self),
        };
        match self.command.is_some() {
            true => Err(<Self as clap::CommandFactory>::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                format!("{flag} goes on bare `efference`, not beside a subcommand"),
            )),
            false => Ok(self),
        }
    }
}

/// `--host` and the flags that only a host takes.
#[derive(clap::Args, Debug, Default, PartialEq, Eq)]
#[command(next_help_heading = "Running a host")]
pub(crate) struct HostFlags {
    /// Run a host: bind a control socket (and optionally a web front door),
    /// start with no sessions, and create them over the socket.
    ///
    /// `POST /sessions` on the socket creates a session from a request body that
    /// carries its working directory and environment; each session's control
    /// API is then at `/sessions/<id>/`. Stays up until stopped.
    #[arg(id = "host", long = "host")]
    pub(crate) run: bool,
    /// Bind the host's control socket here. Defaults to `~/.efference/host.sock`,
    /// which every project's clients look for after their own directory; a
    /// project-local host is `.efference/host.sock`.
    #[arg(long, value_name = "PATH", requires = "host")]
    pub(crate) socket: Option<PathBuf>,
    /// The address browsers open, `https://name[:port]` — the printed link and
    /// the Host allowlist. Defaults to `http://localhost:<port>` on a loopback
    /// bind; required with TLS; differs from the bind behind a proxy that
    /// terminates TLS. Overrides `[host] public-url`.
    #[arg(long, value_name = "URL", requires = "host")]
    pub(crate) public_url: Option<String>,
    /// PEM certificate chain for the bind (the `tailscale cert` output).
    /// Overrides `[host] tls-cert`.
    #[arg(long, value_name = "PEM", requires = "host")]
    pub(crate) tls_cert: Option<PathBuf>,
    /// PEM private key matching the certificate. Overrides `[host] tls-key`.
    #[arg(long, value_name = "PEM", requires = "host")]
    pub(crate) tls_key: Option<PathBuf>,
    /// Open the operator's browser at the sign-in link once bound.
    #[arg(long, requires = "host")]
    pub(crate) open: bool,
    /// Run as a standalone host: stdin is the launcher's ownership
    /// connection, and its closing archives the host's sessions and shuts it down.
    /// Set by the launcher, never typed: a person's
    /// `--host` runs until stopped, and a person without one gets a standalone
    /// host started for them.
    // docs/resumption.md
    #[arg(long, hide = true, requires = "host")]
    pub(crate) island: bool,
}

impl HostFlags {
    /// The web-bind flags, with the global `--bind`, as the host lays them over
    /// its `[host]` table.
    pub(crate) fn bind_flags(&self, bind: Option<String>) -> serve::BindFlags {
        serve::BindFlags {
            bind,
            public_url: self.public_url.clone(),
            tls_cert: self.tls_cert.clone(),
            tls_key: self.tls_key.clone(),
            open: self.open,
        }
    }
}

/// The directory for a session id (`.efference/<id>`).
#[cfg(any(feature = "dev", test))]
pub(crate) fn session_dir(id: &str) -> PathBuf {
    Path::new(SESSION_BASE).join(id)
}

/// The host socket's name under a base dir (docs/control-channel.md).
pub(crate) const HOST_SOCKET_NAME: &str = "host.sock";

/// A project-local host: `.efference/host.sock` here. What `efference --host
/// --socket .efference/host.sock` binds; consulted before the user's host.
pub(crate) fn project_host_socket() -> PathBuf {
    Path::new(SESSION_BASE).join(HOST_SOCKET_NAME)
}

/// The user's host: `~/.efference/host.sock`, what `efference --host` binds
/// and where every project's clients look after their own directory
/// (docs/control-channel.md). `None` without a home directory.
pub(crate) fn user_host_socket() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(SESSION_BASE).join(HOST_SOCKET_NAME))
}

/// Where a client looks for a host, in order: this project's, then the user's.
pub(crate) fn host_candidates() -> Vec<PathBuf> {
    let mut hosts = vec![project_host_socket()];
    hosts.extend(user_host_socket());
    hosts
}

/// The host the new-session form creates on: the first live candidate. `None`
/// is where a terminal-owned island starts instead.
pub(crate) fn live_host() -> Option<PathBuf> {
    host_candidates().into_iter().find(|host| is_live(host))
}

/// The host a mount addresses (`control`, `mcp`): the first live candidate, else
/// the user's host, which need not be running yet — the mount outlives it.
pub(crate) fn mount_host() -> PathBuf {
    let candidates = host_candidates();
    candidates
        .iter()
        .find(|host| is_live(host))
        .cloned()
        .or_else(|| candidates.last().cloned())
        .unwrap_or_else(project_host_socket)
}

/// Where a *client* verb's session lives: a host's socket and the session's
/// prefix under it (docs/control-channel.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientTarget {
    pub(crate) socket: PathBuf,
    /// `/sessions/<id>` on the host socket.
    pub(crate) prefix: String,
}

impl ClientTarget {
    pub(crate) fn hosted(socket: PathBuf, id: &str) -> Self {
        Self {
            socket,
            prefix: format!("/sessions/{id}"),
        }
    }

    pub(crate) fn client(&self) -> ControlClient {
        ControlClient::at(&self.socket, &self.prefix)
    }
}

/// Whether a listener is bound at `socket`: a successful connect, not mere
/// existence, so a leftover file from a dead process never shadows a live one.
pub(crate) fn is_live(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// Whether two paths name the same directory: by canonical form when both
/// resolve, else as spelled. A session's identity carries the cwd its creator
/// sent, which is what `current_dir` returns here when the same directory.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The ids in a host's listing whose cwd is `here`: the project-scoped view of
/// a host that serves several (docs/control-channel.md). A listing without a
/// cwd (a host older than the field) is kept, since it cannot be placed.
pub(crate) fn hosted_here(
    listings: &[efference::client::ListedSession],
    here: &Path,
) -> Vec<String> {
    listings
        .iter()
        .filter(|listing| {
            listing
                .cwd
                .as_ref()
                .is_none_or(|cwd| same_dir(Path::new(cwd), here))
        })
        .map(|listing| listing.id.clone())
        .collect()
}

/// Resolve the session a *client* verb should talk to: the named session, as a
/// host lists it in this directory; else the single session the live hosts list
/// here (docs/control-channel.md). The hosts are [`host_candidates`]: this
/// project's, then the user's. Anything that is not live is an error here, before
/// a UI opens or a call is made.
pub(crate) async fn resolve_client_target(name: Option<&str>) -> Result<ClientTarget, CliError> {
    resolve_client_target_at(host_candidates(), name).await
}

/// [`resolve_client_target`] against the given hosts rather than the default
/// candidates.
pub(crate) async fn resolve_client_target_at(
    hosts: Vec<PathBuf>,
    name: Option<&str>,
) -> Result<ClientTarget, CliError> {
    let here = std::env::current_dir()?;
    // (host, ids it serves in this directory), live hosts only.
    let mut hosted: Vec<(PathBuf, Vec<String>)> = Vec::new();
    let mut down: Vec<String> = Vec::new();
    for host in hosts {
        if !is_live(&host) {
            down.push(host.display().to_string());
            continue;
        }
        let listings = ControlClient::new(&host)
            .sessions()
            .await
            .map_err(|e| format!("listing {}: {e}", host.display()))?;
        hosted.push((host, hosted_here(&listings, &here)));
    }
    if let Some(name) = name {
        if let Some((host, _)) = hosted
            .iter()
            .find(|(_, ids)| ids.iter().any(|id| id == name))
        {
            return Ok(ClientTarget::hosted(host.clone(), name));
        }
        let mut why = Vec::new();
        if !hosted.is_empty() {
            why.push("no host lists it here".to_string());
        }
        if !down.is_empty() {
            why.push(format!("the host at {} is not running", down.join(" or ")));
        }
        return Err(format!("no live session {name:?}: {}", why.join("; ")).into());
    }
    let mut live: Vec<ClientTarget> = Vec::new();
    for (host, ids) in &hosted {
        live.extend(ids.iter().map(|id| ClientTarget::hosted(host.clone(), id)));
    }
    match live.len() {
        0 => Err(
            "no live efference session found here — create one with `efference`, or pass --name"
                .into(),
        ),
        1 => Ok(live.pop().unwrap()),
        _ => {
            let names: Vec<String> = live
                .iter()
                .map(|target| format!("{} ({})", target.socket.display(), target.prefix))
                .collect();
            Err(format!(
                "multiple live sessions ({}); pass --name to choose",
                names.join(", ")
            )
            .into())
        }
    }
}

#[cfg(feature = "dev")]
/// Resolve what a *reader* verb should open: an explicit path (to a `log.jsonl` or
/// to a session directory), else a session name under `.efference/`. Returns the log
/// path and the label to show for it — the session id when the log was reached by
/// name, the path as given otherwise.
///
/// Liveness is not consulted: reading a live session's log is a snapshot of it, which
/// is a thing to allow rather than an error.
pub(crate) fn resolve_session_log(target: &str) -> Result<(PathBuf, String), CliError> {
    let as_path = Path::new(target);
    if as_path.is_file() {
        return Ok((as_path.to_path_buf(), target.to_string()));
    }
    if as_path.is_dir() {
        let log = as_path.join(LOG_NAME);
        if log.is_file() {
            return Ok((log, target.to_string()));
        }
        return Err(format!("{target} holds no {LOG_NAME}").into());
    }
    let log = session_dir(target).join(LOG_NAME);
    if log.is_file() {
        return Ok((log, target.to_string()));
    }
    Err(format!("no session {target:?}.{}", session_listing()).into())
}

#[cfg(feature = "dev")]
/// How many sessions a listing names before it just counts the rest. A long-running
/// project accumulates dozens of session dirs, and an error that prints all of them
/// buries the error.
const LISTED_SESSIONS: usize = 10;

#[cfg(feature = "dev")]
/// The session ids under `.efference/` that have a log, **most recently written
/// first** — what a reader verb offers when it was given no session, or one that
/// doesn't resolve. Ordered by the log's mtime rather than by name, since the
/// default ids are timestamps but a `--name`d session is whatever the operator
/// called it.
pub(crate) fn session_ids() -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(SESSION_BASE) {
        for entry in entries.flatten() {
            let log = entry.path().join(LOG_NAME);
            if let Ok(meta) = log.metadata()
                && meta.is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                found.push((meta.modified().ok(), name.to_string()));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, name)| name).collect()
}

#[cfg(feature = "dev")]
/// The recorded sessions as an error-message tail — the most recent
/// [`LISTED_SESSIONS`] by name, a count for any beyond them, and a plain note when
/// the directory holds none.
pub(crate) fn session_listing() -> String {
    let ids = session_ids();
    if ids.is_empty() {
        return format!(" No recorded sessions under {SESSION_BASE}/.");
    }
    let shown = ids.len().min(LISTED_SESSIONS);
    let mut listing = format!(" Recent sessions: {}", ids[..shown].join(", "));
    if ids.len() > shown {
        listing.push_str(&format!(" (+{} older)", ids.len() - shown));
    }
    listing
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Drive a running session from a script: message it, decide its gates, read it.
    ///
    /// The agent verbs (`create`, `message`, `decide`, `interrupt`, `archive`,
    /// `status`, `transcript`) and `settle`, then the operator's route verbs (`state`,
    /// `pace`, `title`, `background`, `proposal`, `review-level`, `supervisor`,
    /// `notify`, `entries`, `image`). `efference control --help` lists them.
    Control {
        /// The host's control socket every verb addresses; a named session resolves
        /// through it. Defaults to the first live of `.efference/host.sock`
        /// here and `~/.efference/host.sock`, else the latter.
        #[arg(long, value_name = "PATH", global = true)]
        host_socket: Option<PathBuf>,
        #[command(subcommand)]
        tools: efference::control_tools::cli::Tools,
    },
    /// Serve the agent verbs as an MCP server on stdin/stdout.
    ///
    /// The mount another agent harness supervises sessions through: `create`,
    /// `message`, `decide`, `interrupt`, `archive`, `status`, `transcript`, and
    /// `wait`. Speaks newline-delimited JSON-RPC and blocks until stdin closes.
    /// Connects to the host: nothing needs to be running when it starts, and each
    /// call names its agent (`status` lists them, `create` makes one).
    Mcp {
        /// The host's control socket. Defaults to the first live of
        /// `.efference/host.sock` here and `~/.efference/host.sock`, else the latter.
        #[arg(long, value_name = "PATH")]
        host_socket: Option<PathBuf>,
    },
    /// Instruments for developing efference itself.
    ///
    /// The wire view, the element gallery, the spotter and triage testers, the
    /// cache-rate report, the read-only session viewer, and the web-bind
    /// sign-in-link mint. Compiled only under the `dev` cargo feature.
    #[cfg(feature = "dev")]
    #[command(subcommand)]
    Dev(dev::DevCommand),
    /// Configure inference connections interactively: the first-run path.
    ///
    /// Asks which provider, where its credential comes from, and which models,
    /// checking what it can live, and writes the secret-free
    /// `~/.efference/config.toml` that `efference --host` loads. Run again to add a
    /// connection; the file's existing entries and comments are kept.
    Setup {
        /// Write this file instead of `~/.efference/config.toml`.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// The web bind's credentials.
    // docs/web-bind.md
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// The devices signed in to the web bind: each browser that signed in, and each
    /// script given a token, holds one credential that is revoked alone.
    ///
    /// With no verb, lists them: the id (its first 8 hex digits), the kind, how it
    /// joined, when, when it was last seen (recorded at most hourly), and its label;
    /// then the browsers waiting to sign in, by label and age. Asks the running host,
    /// over its socket, and needs its web bind.
    Devices {
        #[command(subcommand)]
        command: Option<devices::DevicesCommand>,
    },
    /// Check whether a newer version of efference is out.
    ///
    /// `--check` asks efference.net once and prints the installed and newest
    /// versions, and, when the newest is newer, the command that installs it. The
    /// answer is believed only when its signature checks against the release key
    /// built into this binary. It asks whether or not `[updates]` turns on the
    /// automatic check.
    // docs/update-check.md
    Update {
        /// Ask efference.net once and print the answer.
        #[arg(long)]
        check: bool,
    },
    /// Validate an efference.toml without starting a session.
    ///
    /// Parses the file, applies the same checks a session boot applies, resolves
    /// its `[context]` entries from the current directory (`exec` entries run), and
    /// mounts each `[mcp.servers.*]` stanza to list the server's tools against the
    /// file's `[tools]` table. Prints what the file grants and how many preamble
    /// bytes it costs; exits non-zero with the boot-time error otherwise. Stored MCP
    /// credentials are reused; a server that needs authorization is reported, not
    /// authorized here.
    Check {
        /// The file to check. Defaults to `efference.toml` in the current directory.
        path: Option<PathBuf>,
    },
}

/// `efference auth <verb>`.
#[derive(Subcommand)]
pub(crate) enum AuthCommand {
    /// Replace the machine root every bind credential derives from.
    ///
    /// Stores a fresh root over the old one, then deletes every enrolled device and
    /// every pending sign-in; the next `efference --host --bind` serves the new
    /// root. This signs out every device, browsers and scripts alike, and drops
    /// every push subscription. Run it when the root may have leaked, when the
    /// secret store reports it unreadable, or, with `[secrets]` commands, to create
    /// the first one; one lost device is `efference devices revoke`.
    Rotate,
}
