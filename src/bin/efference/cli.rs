//! The clap command tree, session-directory layout, and socket resolution.

use super::*;

/// The base directory for all session dirs (gitignored). Each session is `.efference/<id>/`,
/// holding its standard-named socket, log, and config (docs/permissions.md).
pub(crate) const BASE_DIR: &str = efference::daemon::SESSION_BASE;

pub(crate) const SOCKET_NAME: &str = efference::daemon::SOCKET_NAME;

#[cfg(any(feature = "dev", test))]
pub(crate) const LOG_NAME: &str = efference::boot::LOG_NAME;

#[derive(Parser)]
#[command(name = "efference", about = "An API-backed agentic coding harness.")]
pub(crate) struct Cli {
    /// Session name. Defaults to a timestamp (`tui --new`) or the session running at this path.
    #[arg(long, global = true)]
    pub(crate) name: Option<String>,
    /// Explicit control-socket path; overrides `--name` and discovery.
    #[arg(long, global = true)]
    pub(crate) socket: Option<PathBuf>,
    /// Omitted, `efference` runs `setup` when there is no host configuration
    /// yet, and otherwise is `efference tui`: pick a session here, or with no
    /// host running, start one owned by this terminal.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// The directory for a session id (`.efference/<id>`).
pub(crate) fn session_dir(id: &str) -> PathBuf {
    Path::new(BASE_DIR).join(id)
}

/// The host socket's name under a base dir (docs/control-channel.md).
pub(crate) const HOST_SOCKET_NAME: &str = "host.sock";

/// A project-local host: `.efference/host.sock` here. What `efference host
/// --socket .efference/host.sock` binds; consulted before the user's host.
pub(crate) fn project_host_socket() -> PathBuf {
    Path::new(BASE_DIR).join(HOST_SOCKET_NAME)
}

/// The user's host: `~/.efference/host.sock`, what a bare `efference host` binds
/// and where every project's clients look after their own directory
/// (docs/control-channel.md). `None` without a home directory.
pub(crate) fn user_host_socket() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(BASE_DIR).join(HOST_SOCKET_NAME))
}

/// Where a client looks for a host, in order: this project's, then the user's.
pub(crate) fn host_candidates() -> Vec<PathBuf> {
    let mut hosts = vec![project_host_socket()];
    hosts.extend(user_host_socket());
    hosts
}

/// The host a verb that needs one live now talks to (`tui --new`): the first
/// live candidate. `None` is where a terminal-owned island starts instead.
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

/// Where a *client* verb's session lives: a socket, and the prefix under it when
/// the session is one of several a host serves (docs/control-channel.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientTarget {
    pub(crate) socket: PathBuf,
    /// `/sessions/<id>` on a host socket; `None` for a socket that is one session.
    pub(crate) prefix: Option<String>,
}

impl ClientTarget {
    fn socket(socket: PathBuf) -> Self {
        Self {
            socket,
            prefix: None,
        }
    }

    pub(crate) fn hosted(socket: PathBuf, id: &str) -> Self {
        Self {
            socket,
            prefix: Some(format!("/sessions/{id}")),
        }
    }

    pub(crate) fn client(&self) -> ControlClient {
        match &self.prefix {
            Some(prefix) => ControlClient::at(&self.socket, prefix),
            None => ControlClient::new(&self.socket),
        }
    }

    /// The session dir the attachment store lives beside (docs/inbox.md): the
    /// socket's own dir, or `.efference/<id>` here for a hosted session — the
    /// host mints under the session's cwd, not beside its own socket
    /// (docs/control-channel.md), and a hosted target is only ever resolved for
    /// a session whose cwd is this directory. `None` for an explicit socket with
    /// no parent.
    pub(crate) fn session_dir(&self) -> Option<PathBuf> {
        match &self.prefix {
            Some(prefix) => Some(session_dir(prefix.rsplit('/').next()?)),
            None => self.socket.parent().map(Path::to_path_buf),
        }
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
    listings: &[efference::client::SessionSummary],
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

/// Resolve the session a *client* verb should talk to: an explicit `--socket`; else
/// the named session — its own live socket, or a host that lists that id in this
/// directory; else the single live session found by scanning the session dirs and
/// asking every live host for its sessions here (docs/permissions.md). The hosts
/// are [`host_candidates`]: this project's, then the user's. Anything that is not
/// live is an error here, before a UI opens or a call is made.
pub(crate) async fn resolve_client_target(
    name: Option<&str>,
    socket: Option<PathBuf>,
) -> Result<ClientTarget, CliError> {
    resolve_client_target_at(host_candidates(), name, socket).await
}

/// [`resolve_client_target`] against the given hosts rather than the default
/// candidates — what `efference mcp --host-socket` resolves through.
pub(crate) async fn resolve_client_target_at(
    hosts: Vec<PathBuf>,
    name: Option<&str>,
    socket: Option<PathBuf>,
) -> Result<ClientTarget, CliError> {
    if let Some(socket) = socket {
        return Ok(ClientTarget::socket(socket));
    }
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
        let own = session_dir(name).join(SOCKET_NAME);
        if is_live(&own) {
            return Ok(ClientTarget::socket(own));
        }
        if let Some((host, _)) = hosted
            .iter()
            .find(|(_, ids)| ids.iter().any(|id| id == name))
        {
            return Ok(ClientTarget::hosted(host.clone(), name));
        }
        return Err(format!(
            "no live session {name:?}: nothing listens at {}{}{}",
            own.display(),
            if hosted.is_empty() {
                String::new()
            } else {
                " and no host lists it here".to_string()
            },
            if down.is_empty() {
                String::new()
            } else {
                format!("; the host at {} is not running", down.join(" or "))
            }
        )
        .into());
    }
    let mut live: Vec<ClientTarget> = discover_live_sockets()?
        .into_iter()
        .map(ClientTarget::socket)
        .collect();
    for (host, ids) in &hosted {
        live.extend(ids.iter().map(|id| ClientTarget::hosted(host.clone(), id)));
    }
    match live.len() {
        0 => Err(
            "no live efference session found here — create one with `tui --new`, or pass --name/--socket"
                .into(),
        ),
        1 => Ok(live.pop().unwrap()),
        _ => {
            let names: Vec<String> = live
                .iter()
                .map(|target| match &target.prefix {
                    Some(prefix) => format!("{} ({})", target.socket.display(), prefix),
                    None => target.socket.display().to_string(),
                })
                .collect();
            Err(format!(
                "multiple live sessions ({}); pass --name to choose",
                names.join(", ")
            )
            .into())
        }
    }
}

/// Every `.efference/*/control.sock` with a listener bound, sorted.
fn discover_live_sockets() -> Result<Vec<PathBuf>, CliError> {
    let base = Path::new(BASE_DIR);
    let mut live = Vec::new();
    if base.is_dir() {
        for entry in std::fs::read_dir(base)? {
            let sock = entry?.path().join(SOCKET_NAME);
            if is_live(&sock) {
                live.push(sock);
            }
        }
    }
    live.sort();
    Ok(live)
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
    if let Ok(entries) = std::fs::read_dir(BASE_DIR) {
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
        return format!(" No recorded sessions under {BASE_DIR}/.");
    }
    let shown = ids.len().min(LISTED_SESSIONS);
    let mut listing = format!(" Recent sessions: {}", ids[..shown].join(", "));
    if ids.len() > shown {
        listing.push_str(&format!(" (+{} older)", ids.len() - shown));
    }
    listing
}

/// The session request as a person types it — the flags `tui --new`
/// share, one per `SessionRequest` field (docs/control-api.md). Listener flags
/// (`--bind` and its family) are not here: they belong to whoever binds, never
/// to a session.
#[derive(clap::Args, Default, PartialEq, Eq)]
pub(crate) struct SessionFlags {
    /// Model for the agent, as `<connection>/<native model>` or a native id on the
    /// host's default connection. Omitted, the host's `[defaults].model` applies
    /// (~/.efference/config.toml).
    #[arg(long)]
    pub(crate) model: Option<String>,
    /// Model for the spotter permission rung, selected like --model.
    ///
    /// Independent of the agent model — the spotter is volume work, so it defaults to
    /// the host's `[defaults].spotter`, else the agent connection's cheap band:
    /// claude-haiku-4-5 on Anthropic (anthropic/claude-haiku-4-5 on OpenRouter),
    /// gpt-5.6-luna on OpenAI and OpenAI-Codex.
    #[arg(long)]
    pub(crate) spotter_model: Option<String>,
    /// Use the main client for the fork peek even when `[defaults].triage` configures
    /// a separate model.
    #[arg(
        long,
        conflicts_with_all = ["triage_model", "triage_thinking", "triage_auto_choice", "triage_effort"]
    )]
    pub(crate) triage_shared: bool,
    /// Model for the fork peek that triages every tool result, selected like --model.
    ///
    /// Experimental. Omitted, the peek runs on the agent's own client and reads the
    /// agent's prompt cache (on a host, unless `[defaults].triage` is set). Given, the
    /// per-result peek runs on its own client even when it names the agent's model:
    /// its own system prompt, its own session key, and only the two peek tools
    /// offered. Its cache is a separate lineage over the same transcript, paid at
    /// that model's rate. Must share the agent's wire dialect. The idle triage (the
    /// wake decision) stays on the agent's client regardless.
    #[arg(long)]
    pub(crate) triage_model: Option<String>,
    /// Let the triage model think. Off by default: the peek's reasoning is discarded
    /// with the fork, so thinking there is latency and tokens per tool result. Only
    /// meaningful with --triage-model.
    #[arg(long)]
    pub(crate) triage_thinking: bool,
    /// Send the triage model's ask under `tool_choice: auto` instead of forcing the
    /// peek tool. Forced is the default: an unforced small model answers the ask
    /// with an invented work call. Only meaningful with --triage-model.
    #[arg(long)]
    pub(crate) triage_auto_choice: bool,
    /// Reasoning level for the triage model, in its own vocabulary (validated like
    /// --effort). Implies --triage-thinking: for a model that can be turned down
    /// rather than off.
    #[arg(long, value_name = "LEVEL")]
    pub(crate) triage_effort: Option<String>,
    /// Path to the config file. Defaults to `efference.toml` in the current directory.
    ///
    /// With no file present the built-in default config is used. An explicit path that doesn't exist is an error. The resolved path is
    /// reported on `GET /state`.
    #[arg(long, short = 'c')]
    pub(crate) config: Option<PathBuf>,
    /// Sampling temperature on a 0–100 scale; 50 is each provider's default.
    ///
    /// Integer to avoid float serialization drift and the two-provider-scale confusion: 50 is mid-scale
    /// and maps to each provider's default (1.0). Omitted → the field is left out
    /// of the wire entirely, and the API uses its own default (1.0).
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub(crate) temperature: Option<u8>,
    /// How much effort the model spends per turn, e.g. high, xhigh, max.
    ///
    /// Accepted as given and validated at boot against
    /// the levels the model's metadata declares — an unsupported level is
    /// refused there with the list, because OpenRouter maps one to the nearest
    /// supported silently. Overrides `[transport] effort` in the config;
    /// omitted with no config key → the field is left off the wire and the
    /// provider's own default stands (the boot line prints it when the
    /// metadata names one). Boot-time only — there is no runtime toggle, since
    /// changing a thinking parameter mid-session invalidates the message-tier
    /// cache. `xhigh`/`max` require thinking, so pairing either with
    /// EFFERENCE_THINKING=off is refused at startup.
    #[arg(long, value_name = "LEVEL")]
    pub(crate) effort: Option<String>,
    /// Pin which upstream endpoint serves the model, for an OpenRouter session.
    ///
    /// An endpoint slug such as `google-ai-studio` or `google-vertex/global`
    /// (OpenRouter's model page lists them). Repeatable — the
    /// order is the preference order, and fallbacks past it are disabled, so
    /// a pinned session either gets one of these or fails rather than
    /// silently rerouting.
    ///
    /// Distinct from the connection's declared provider, which is selected
    /// by session configuration. Ignored on Anthropic, which has no such
    /// choice. Applies to the agent's own requests (turns and fork peeks),
    /// not the spotter: slugs are per-model and the spotter runs a different
    /// one. Endpoints behind one model slug differ in uptime and latency
    /// tail, so a controlled run should pin one and record it.
    #[arg(long, value_name = "SLUG")]
    pub(crate) upstream: Vec<String>,
    /// Run with no human at the approval gate.
    ///
    /// A spotter `Watch`/`Gate` becomes a redirect that hands the call back to the model, pointing it at its config
    /// file to request access, instead of blocking on an absent human. A config-file write
    /// still gates — held for the orchestrator above to review over the control channel.
    #[arg(long)]
    pub(crate) autonomous: bool,
    /// Re-authorize this MCP server, discarding its stored credentials first. Repeatable.
    ///
    /// The recovery path when a grant
    /// has been revoked or the server has changed authorization servers — otherwise
    /// a stored token is reused until it fails.
    #[arg(long, value_name = "SERVER")]
    pub(crate) reauth: Vec<String>,
    /// Keep the per-session `$TMPDIR` scratch directory on exit, for post-mortem analysis.
    #[arg(long)]
    pub(crate) keep_scratch: bool,
}

impl SessionFlags {
    /// The tagged triage choice the creation request carries.
    pub(crate) fn triage(&self) -> Option<TriageChoice> {
        if self.triage_shared {
            Some(TriageChoice::Shared)
        } else {
            self.triage_model.clone().map(|model| {
                TriageChoice::separate(
                    model,
                    self.triage_thinking,
                    self.triage_auto_choice,
                    self.triage_effort.clone(),
                )
            })
        }
    }

    /// Whether any session flag was given, for a verb that only accepts them in
    /// one mode.
    pub(crate) fn any_given(&self) -> bool {
        *self != Self::default()
    }
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Host sessions in this process: bind a control socket (and optionally a web
    /// front door), start with none, and create them over the socket.
    ///
    /// `POST /sessions` on the socket creates a session from a request body that
    /// carries its working directory and environment; each session's control API is
    /// then at `/sessions/<id>/`.
    Host {
        /// Also listen on a TCP address: the web client's front door. Overrides
        /// `[host] bind` in ~/.efference/config.toml; `--bind ""` serves the Unix
        /// socket alone whatever the table says.
        #[arg(long, value_name = "ADDR:PORT")]
        bind: Option<String>,
        /// The name browsers address the bind by — the Host allowlist and the
        /// hostname in the printed link. Defaults to `localhost` on a loopback bind;
        /// required with TLS. Overrides `[host] public-name`.
        #[arg(long, value_name = "NAME")]
        public_name: Option<String>,
        /// PEM certificate chain for the bind (the `tailscale cert` output).
        /// Overrides `[host] tls-cert`.
        #[arg(long, value_name = "PEM")]
        tls_cert: Option<PathBuf>,
        /// PEM private key matching the certificate. Overrides `[host] tls-key`.
        #[arg(long, value_name = "PEM")]
        tls_key: Option<PathBuf>,
        /// Open the operator's browser at the bootstrap link once bound.
        #[arg(long)]
        open: bool,
        /// Run as a terminal-owned island: stdin is the launcher's ownership
        /// connection, and its closing shuts the host down (docs/control-channel.md).
        /// Set by the launcher, not typed.
        #[arg(long, hide = true)]
        island: bool,
    },
    /// Drive a running session from a script: send turns, resolve approvals, read state.
    ///
    /// One subcommand per control-API call. These are the same tools `efference mcp`
    /// serves over MCP, so a shell script and a mounting harness reach a session
    /// through one definition. `efference control --help` lists them.
    #[command(subcommand)]
    Control(efference::control_tools::Tools),
    /// Serve the `control` tools as an MCP server on stdin/stdout.
    ///
    /// The mount another agent harness supervises sessions through: each `control`
    /// subcommand is one tool. Speaks newline-delimited JSON-RPC and blocks until
    /// stdin closes. Connects to the host: nothing needs to be running when it
    /// starts, and each call names its session (`sessions` lists them, `create`
    /// makes one). With `--name` or `--socket`, a call that names no session
    /// drives that one.
    Mcp {
        /// The host's control socket. Defaults to the first live of
        /// `.efference/host.sock` here and `~/.efference/host.sock`, else the latter.
        #[arg(long, value_name = "PATH")]
        host_socket: Option<PathBuf>,
    },
    /// Instruments for developing efference itself.
    ///
    /// The wire view, the element gallery, the spotter and peek testers, the
    /// cache-rate report, the read-only session viewer, and the web-bind
    /// bootstrap-link mint. Compiled only under the `dev` cargo feature.
    #[cfg(feature = "dev")]
    #[command(subcommand)]
    Dev(dev::DevCommand),
    /// efference in your terminal: attach to a running session, or create one on a host.
    ///
    /// The terminal counterpart to the web client. Shows the transcript live, with a
    /// composer for turns and approvals. Press ? inside for the keys.
    ///
    /// With no flags it asks first: an inline list of the host's sessions in this
    /// directory, and an entry that creates one here. `--name <id>` attaches to that
    /// session without asking.
    ///
    /// With `--new`, creates a session on a running `efference host` first — this
    /// directory as its working root, this shell's environment as its base, `--name`
    /// as its id — and attaches to it. Quitting
    /// detaches; the session keeps running on the host.
    Tui {
        /// Create a session on the host at --host-socket, then attach to it. With
        /// no host running, starts one owned by this terminal, which ends with it.
        #[arg(long)]
        new: bool,
        /// The host's control socket. Defaults to the first live of
        /// `.efference/host.sock` here and `~/.efference/host.sock`.
        #[arg(long, value_name = "PATH", requires = "new")]
        host_socket: Option<PathBuf>,
        #[command(flatten)]
        session: SessionFlags,
        /// Syntax/UI theme: a built-in theme name (e.g. "Catppuccin Frappe",
        /// "Nord", "gruvbox-dark") or a path to a `.tmTheme` file. An unknown name
        /// lists the available ones. Defaults to "Catppuccin Frappe".
        #[arg(long, env = "EFFERENCE_THEME")]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables.
        #[arg(long, env = "EFFERENCE_IMAGES", value_enum, default_value_t)]
        images: ImagesArg,
        /// Time each paint and print the distribution (p50/p90/p99/max, and how
        /// many frames ran over the 30ms tick) on exit. Development
        /// instrumentation for the streaming render.
        #[arg(long)]
        frame_cost: bool,
    },
    /// Configure inference connections interactively: the first-run path.
    ///
    /// Asks which provider, where its credential comes from, and which models,
    /// checking what it can live, and writes the secret-free
    /// `~/.efference/config.toml` that `efference host` loads. Run again to add a
    /// connection; the file's existing entries and comments are kept.
    Setup {
        /// Write this file instead of `~/.efference/config.toml`.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// The web bind's credentials (docs/web-bind.md).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Validate an efference.toml without starting a session.
    ///
    /// Parses the file, applies the same checks a session boot applies, resolves
    /// its `[context]` entries from the current directory (`exec` entries run), and
    /// mounts each `[mcp.servers.*]` stanza to list the server's tools against the
    /// file's `tools` grants. Prints what the file grants and how many preamble
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
    /// Deletes the stored root; the next `efference host --bind` mints a new one.
    /// This signs out every browser, invalidates the authenticator enrollment,
    /// and drops every push subscription. Run it when the root may have leaked,
    /// or when the secret store reports it unreadable.
    Rotate,
}

/// The `--images` value for the TUI verbs — the CLI face of
/// [`efference::tui::ImageMode`] (docs/tui.md). Detection lies under tmux in
/// both directions, so the override covers both: forcing a protocol detection
/// missed, and turning off one it wrongly claimed.
#[derive(Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum ImagesArg {
    /// Query the terminal; halfblocks when nothing answers.
    #[default]
    Auto,
    /// Force the kitty graphics protocol.
    Kitty,
    /// Force iTerm2's OSC 1337.
    Iterm2,
    /// No image rendering — an image shows as its one-line reference.
    Off,
}

impl From<ImagesArg> for efference::tui::ImageMode {
    fn from(arg: ImagesArg) -> Self {
        use efference::tui::ImageMode;
        match arg {
            ImagesArg::Auto => ImageMode::Auto,
            ImagesArg::Kitty => ImageMode::Kitty,
            ImagesArg::Iterm2 => ImageMode::Iterm2,
            ImagesArg::Off => ImageMode::Off,
        }
    }
}
