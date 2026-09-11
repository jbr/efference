//! The clap command tree, session-directory layout, and socket resolution.

use super::*;

/// The base directory for all session dirs (gitignored). Each session is `.efference/<id>/`,
/// holding its standard-named socket, log, and config (docs/permissions.md).
pub(crate) const BASE_DIR: &str = efference::daemon::SESSION_BASE;

pub(crate) const SOCKET_NAME: &str = efference::daemon::SOCKET_NAME;

pub(crate) const LOG_NAME: &str = efference::boot::LOG_NAME;

#[derive(Parser)]
#[command(name = "efference", about = "An API-backed agentic coding harness.")]
pub(crate) struct Cli {
    /// Session name. Defaults to a timestamp (`serve`) or the session running at this path.
    #[arg(long, global = true)]
    pub(crate) name: Option<String>,
    /// Explicit control-socket path; overrides `--name` and discovery.
    #[arg(long, global = true)]
    pub(crate) socket: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// The directory for a session id (`.efference/<id>`).
pub(crate) fn session_dir(id: &str) -> PathBuf {
    Path::new(BASE_DIR).join(id)
}

/// A default session id when `--name` is omitted: timestamp millis — sortable, human-meaningful,
/// and matching the old flat `session-<millis>` naming.
pub(crate) fn default_session_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis()
        .to_string()
}

/// Resolve the control socket a *client* verb should talk to: an explicit `--socket`, else the
/// named session's socket, else the single **live** daemon discovered by scanning the session dirs
/// (docs/permissions.md).
pub(crate) fn resolve_client_socket(
    name: Option<&str>,
    socket: Option<PathBuf>,
) -> Result<PathBuf, CliError> {
    if let Some(socket) = socket {
        return Ok(socket);
    }
    if let Some(name) = name {
        return Ok(session_dir(name).join(SOCKET_NAME));
    }
    discover_live_socket()
}

/// Scan `.efference/*/control.sock` for a **live** (connectable) socket — liveness, not mere
/// existence, so a stale dir from a dead daemon never shadows the running one. A successful
/// `UnixStream::connect` means a listener is bound; a leftover socket file with no listener refuses
/// the connection. Exactly one live daemon → use it; zero or several → a helpful error.
pub(crate) fn discover_live_socket() -> Result<PathBuf, CliError> {
    let base = Path::new(BASE_DIR);
    let mut live = Vec::new();
    if base.is_dir() {
        for entry in std::fs::read_dir(base)? {
            let sock = entry?.path().join(SOCKET_NAME);
            if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                live.push(sock);
            }
        }
    }
    match live.len() {
        0 => Err(
            "no live efference daemon found — start one with `serve`, or pass --name/--socket"
                .into(),
        ),
        1 => Ok(live.pop().unwrap()),
        _ => {
            live.sort();
            let names: Vec<String> = live.iter().map(|p| p.display().to_string()).collect();
            Err(format!(
                "multiple live daemons ({}); pass --name to choose",
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

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Start a session: run the agent loop in this directory.
    ///
    /// The session daemon binds a control socket under `.efference/<id>/` and runs the
    /// whole agentic loop — turns, tool calls, approvals — for as long as it lives.
    /// Everything else talks to it over that socket: `efference tui` in a terminal, the
    /// web client over `--bind`, and `efference control`/`efference mcp` for scripts and
    /// other agents. Nothing happens until a client sends the first turn.
    Serve {
        /// Model to use for the agent. Defaults to claude-haiku-4-5.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Model for the spotter permission rung.
        ///
        /// Independent of the agent model — the spotter is volume work, so it defaults to the
        /// provider's cheap band regardless of what the agent runs: claude-haiku-4-5 on
        /// Anthropic and OpenRouter, gpt-5.6-luna on OpenAI and OpenAI-Codex.
        #[arg(long)]
        spotter_model: Option<String>,
        /// Model for the fork peek that triages every tool result.
        ///
        /// Experimental. Omitted, or equal to --model, the peek runs on the agent's own
        /// client and reads the agent's prompt cache. A different model runs the peek
        /// on its own client: its own system prompt, its own session key, and only the
        /// two peek tools offered. Its cache is a separate lineage over the same
        /// transcript, paid at that model's rate.
        #[arg(long)]
        triage_model: Option<String>,
        /// Let the triage model think. Off by default: the peek's reasoning is discarded
        /// with the fork, so thinking there is latency and tokens per tool result. Only
        /// meaningful with --triage-model.
        #[arg(long)]
        triage_thinking: bool,
        /// Send the triage model's ask under `tool_choice: auto` instead of forcing the
        /// peek tool. Forced is the default: an unforced small model answers the ask
        /// with an invented work call. Only meaningful with --triage-model.
        #[arg(long)]
        triage_auto_choice: bool,
        /// Reasoning level for the triage model, in its own vocabulary (validated like
        /// --effort). Implies --triage-thinking: for a model that can be turned down
        /// rather than off.
        #[arg(long, value_name = "LEVEL")]
        triage_effort: Option<String>,
        /// Path to the config file. Defaults to `efference.toml` in the current directory.
        ///
        /// With no file present the built-in default config is used. An explicit path that doesn't exist is an error. The resolved path is
        /// reported on `GET /state`.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Sampling temperature on a 0–100 scale; 50 is each provider's default.
        ///
        /// Integer to avoid float serialization drift and the two-provider-scale confusion: 50 is mid-scale
        /// and maps to each provider's default (1.0). Omitted → the field is left out
        /// of the wire entirely, and the API uses its own default (1.0).
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
        temperature: Option<u8>,
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
        effort: Option<String>,
        /// Pin which upstream endpoint serves the model, for an OpenRouter session.
        ///
        /// An endpoint slug such as `google-ai-studio` or `google-vertex/global`
        /// (OpenRouter's model page lists them). Repeatable — the
        /// order is the preference order, and fallbacks past it are disabled, so
        /// a pinned session either gets one of these or fails rather than
        /// silently rerouting.
        ///
        /// Distinct from the anthropic-vs-openrouter choice, which is the
        /// `EFFERENCE_PROVIDER` env var. Ignored on Anthropic, which has no such
        /// choice. Applies to the agent's own requests (turns and fork peeks),
        /// not the spotter: slugs are per-model and the spotter runs a different
        /// one. Endpoints behind one model slug differ in uptime and latency
        /// tail, so a controlled run should pin one and record it.
        #[arg(long, value_name = "SLUG")]
        upstream: Vec<String>,
        /// Run with no human at the approval gate.
        ///
        /// A spotter `Watch`/`Gate` becomes a redirect that hands the call back to the model, pointing it at its config
        /// file to request access, instead of blocking on an absent human. A config-file write
        /// still gates — held for the orchestrator above to review over the control channel.
        #[arg(long)]
        autonomous: bool,
        /// Re-authorize this MCP server, discarding its stored credentials first. Repeatable.
        ///
        /// The recovery path when a grant
        /// has been revoked or the server has changed authorization servers — otherwise
        /// a stored token is reused until it fails.
        #[arg(long, value_name = "SERVER")]
        reauth: Vec<String>,
        /// Keep the per-session `$TMPDIR` scratch directory on exit, for post-mortem analysis.
        #[arg(long)]
        keep_scratch: bool,
        /// Also listen on a TCP address: the web client's front door.
        ///
        /// Loopback may be plaintext; any other address
        /// requires --tls-cert/--tls-key (never cleartext on a routable interface),
        /// and either way EFFERENCE_ROOT_SECRET must be set (the op env-file
        /// pattern). Deliberately a flag/env, not a config key: the session config
        /// is model-proposable, and network exposure must not be. The env fallbacks
        /// on this flag family are for a direnv'd default (the operator's shell),
        /// same trust as typing them; `--bind ""` overrides such a default back to
        /// UDS-only (a second concurrent session, or the port is busy).
        #[arg(long, value_name = "ADDR:PORT", env = "EFFERENCE_BIND")]
        bind: Option<String>,
        /// The public name browsers address the bind by — the Host allowlist and the
        /// hostname in the printed link (your ts.net name). Defaults to `localhost`
        /// on a loopback bind; required with TLS.
        #[arg(long, value_name = "NAME", requires = "bind", env = "EFFERENCE_HOST")]
        host: Option<String>,
        /// PEM certificate chain for the bind (the `tailscale cert` output).
        #[arg(
            long,
            value_name = "PEM",
            requires = "tls_key",
            env = "EFFERENCE_TLS_CERT"
        )]
        tls_cert: Option<PathBuf>,
        /// PEM private key matching --tls-cert.
        #[arg(
            long,
            value_name = "PEM",
            requires = "tls_cert",
            env = "EFFERENCE_TLS_KEY"
        )]
        tls_key: Option<PathBuf>,
        /// Open the operator's browser at the bootstrap link once bound.
        #[arg(long, requires = "bind")]
        open: bool,
    },
    /// Drive a running session from a script: send turns, resolve approvals, read state.
    ///
    /// One subcommand per control-API call. These are the same tools `efference mcp`
    /// serves over MCP, so a shell script and a mounting harness reach a session
    /// through one definition. `efference control --help` lists them.
    #[command(subcommand)]
    Control(control::Tools),
    /// Serve the `control` tools as an MCP server on stdin/stdout.
    ///
    /// The mount another agent harness supervises a session through: each `control`
    /// subcommand is one tool. Speaks newline-delimited JSON-RPC and blocks until
    /// stdin closes. Pass `--name` to pick the session when several are live.
    Mcp,
    /// Instruments for developing efference itself.
    ///
    /// The wire view, the element gallery, the spotter and peek testers, the
    /// cache-rate report, the read-only session viewer, and the web-bind
    /// bootstrap-link mint. Compiled only under the `dev` cargo feature.
    #[cfg(feature = "dev")]
    #[command(subcommand)]
    Dev(dev::DevCommand),
    /// efference in your terminal: attach to a running session.
    ///
    /// The terminal counterpart to the web client. Connects to the session `serve`
    /// started here (or the one named by `--name`) and shows the transcript live,
    /// with a composer for turns and approvals. Press ? inside for the keys.
    Tui {
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
    /// Validate an efference.toml without starting a session.
    ///
    /// Parses the file, applies the same checks `serve` applies at boot, resolves
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
