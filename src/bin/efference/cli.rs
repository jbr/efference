//! The clap command tree, session-directory layout, and socket resolution.

use super::*;

/// The base directory for all session dirs (gitignored). Each session is `.efference/<id>/`,
/// holding its standard-named socket, log, and config (docs/permissions.md).
pub(crate) const BASE_DIR: &str = efference::daemon::SESSION_BASE;

pub(crate) const SOCKET_NAME: &str = efference::daemon::SOCKET_NAME;

pub(crate) const LOG_NAME: &str = "log.jsonl";

pub(crate) const CONFIG_NAME: &str = "config.toml";

/// Written into a fresh session's `config.toml` when there's no origin to snapshot — a header that
/// teaches the schema, plus the one default that isn't empty: an `Environment at boot` `[context]`
/// exec entry, orientation expressed as config rather than hardcoded (docs/context.md). It's a real,
/// visible line in the session's own file, so a session that `cat`s its config sees it and an
/// operator can edit or delete it (docs/permissions.md).
pub(crate) const CONFIG_SCAFFOLD: &str = "\
# Your session's permissions config. Edit it to request changes — edits are reviewed before they
# take effect, and a malformed file is rejected wholesale (fail-closed): if an edit doesn't seem to
# \"take\", check that the file still parses. Unknown keys are rejected. Schema (all keys optional):
#   scope = \"...\"                       # STRING, top-level — what this session is for (NOT a [scope] table)
#   watch-for = [\"...\"]                 # hazards the permission spotter should watch for
#   trust-threshold = 0.5               # where the watch-window/gate cutoffs sit (optional float)
#   tools = [\"read\", \"list\", \"search\"]  # structured tools you may use
#   [commands]                          # bash prefixes → clear/watch/gate/deny (true = clear, false = deny),
#                                       #   e.g. cargo = true, \"git diff\" = true, \"git push\" = \"deny\".
#                                       #   The longest matching prefix governs, so a broad grant can carry
#                                       #   carve-outs: git = true with \"git push\" = \"deny\".
#   [paths]                             # filesystem grants, e.g. \".\" = true or \"/refs\" = { read = true }
#   [context]                           # knowledge the session boots with (docs/context.md), e.g.
#                                       #   \"CLAUDE.md\" = true, Label = { exec = \"...\" } (stdout at boot),
#                                       #   or Label = { builtin = \"guide\" } (the harness's own docs)
#   [env]                               # per-variable env policy (docs/env.md): strip = keep it out of
#                                       #   spawned children, mask = never store its value in the log.
#                                       #   e.g. \"*_KEY\" = { mask = true } or \"CI_*\" = { strip = true }.
#                                       #   *_KEY/*_SECRET/*_TOKEN/*_PASSWORD/*_CREDENTIALS mask by default.
#                                       #   An entry may also name the value: RUST_LOG = { value = \"debug\" }
#                                       #   (literal), SOME_KEY = \"op://Vault/Item/field\" (resolved at boot
#                                       #   and always masked), or { cmd = \"...\" } for any other vault.

# The one command a session with no project config needs cleared: validating the template it
# is about to write (docs/operating.md). Everything else goes through the spotter until granted.
[commands]
\"efference check\" = true

# Orientation as config: this section's stdout is frozen into the session preamble at boot.
[context]
\"Environment at boot\" = { exec = 'echo \"working directory: $PWD\"; echo \"platform: $(uname -sm)\"; echo \"top-level entries:\"; ls' }
# The harness's own usage documents, listed as read-on-demand stubs. This is what makes a
# session with no project config recoverable: it starts knowing how to learn everything
# else, including how to write this file. Delete the line to stop listing them; the
# documents stay readable at their paths either way.
EfferenceGuide = { builtin = \"guide\" }
";

#[derive(Parser)]
#[command(name = "efference", about = "An API-backed agentic coding harness.")]
pub(crate) struct Cli {
    /// Session name. `serve` creates `.efference/<name>/`; a client verb locates that session's
    /// socket. Omitted: `serve` generates a timestamp id; a client uses the single live daemon
    /// (discovered by scanning the session dirs), erroring if several are live.
    #[arg(long, global = true)]
    pub(crate) name: Option<String>,
    /// Explicit control-socket path — an escape hatch that overrides `--name` and discovery.
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

/// How many sessions a listing names before it just counts the rest. A long-running
/// project accumulates dozens of session dirs, and an error that prints all of them
/// buries the error.
const LISTED_SESSIONS: usize = 10;

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
    /// Run the daemon: bind the control socket and drive the agent loop. Waits for
    /// the first user turn over the socket (`POST /turns`).
    Serve {
        /// Model to use for the agent. Defaults to claude-haiku-4-5.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Model for the spotter permission rung (docs/permissions.md). Independent
        /// of the agent model — the spotter is volume work, so it defaults to the
        /// cheap band (Haiku) regardless of what the agent runs.
        #[arg(long, default_value = "claude-haiku-4-5")]
        spotter_model: String,
        /// Path to the config (TOML, see docs/permissions.md). If omitted,
        /// `efference.toml` in the cwd is probed; absent → the built-in default config.
        /// An explicit path that doesn't exist is an error. The resolved path is
        /// reported on `GET /state`.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Sampling temperature on a 0–100 scale. Integer to avoid float
        /// serialization drift and the two-provider-scale confusion: 50 is mid-scale
        /// and maps to each provider's default (1.0). Omitted → the field is left out
        /// of the wire entirely, and the API uses its own default (1.0).
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
        temperature: Option<u8>,
        /// How much effort the model spends per turn, e.g. high, xhigh, max
        /// (docs/transport.md). Accepted as given and validated at boot against
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
        /// Pin which **upstream** serves the model, for an OpenRouter session:
        /// an endpoint slug such as `google-ai-studio` or `google-vertex/global`
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
        /// tail, so a controlled run should pin one and record it
        /// (eval/harness/README.md).
        #[arg(long, value_name = "SLUG")]
        upstream: Vec<String>,
        /// Run **autonomous** (no synchronous human at the gate): a spotter `Watch`/`Gate`
        /// becomes a redirect that hands the call back to the model, pointing it at its config
        /// file to request access, instead of blocking on an absent human. A config-file write
        /// still gates — held for the orchestrator-above to review over the control channel
        /// (docs/permissions.md).
        #[arg(long)]
        autonomous: bool,
        /// Re-run the MCP authorization dance for this server, discarding any stored
        /// credentials for it first (docs/mcp-auth.md). The recovery path when a grant
        /// has been revoked or the server has changed authorization servers — otherwise
        /// a stored token is reused until it fails. Repeatable.
        #[arg(long, value_name = "SERVER")]
        reauth: Vec<String>,
        /// Keep the per-session `$TMPDIR` scratch directory on exit instead of
        /// deleting it — an artifact for post-mortem analysis (docs/tools.md).
        #[arg(long)]
        keep_scratch: bool,
        /// Also listen on a TCP address (docs/web-bind.md) — the web client's front
        /// door, beside the UDS. Loopback may be plaintext; any other address
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
    /// Run the host: one authenticated front door for every session on this machine
    /// (docs/host.md). Serves the live-session listing at `GET /sessions`, each
    /// session's own control API under `/sessions/<id>/`, and the web client at the
    /// root. It starts no sessions and adopts whatever `serve` processes are running
    /// under `.efference/` in this directory; their Unix sockets stay UDS-only.
    ///
    /// The bind and auth flags are `serve --bind`'s, with the same env fallbacks and
    /// the same posture: plaintext only on loopback, TLS wants an explicit public
    /// name, and EFFERENCE_ROOT_SECRET must be set.
    Host {
        /// The address to listen on. Required — a host with no listener is nothing.
        #[arg(long, value_name = "ADDR:PORT", env = "EFFERENCE_BIND")]
        bind: Option<String>,
        /// The public name browsers address the host by — the Host allowlist and the
        /// hostname in the printed link. Defaults to `localhost` on a loopback bind;
        /// required with TLS.
        #[arg(long, value_name = "NAME", env = "EFFERENCE_HOST")]
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
        #[arg(long)]
        open: bool,
    },
    /// Mint a fresh bootstrap link against a running daemon's TCP bind and open it —
    /// the re-auth path when at the host, and the answer to lost scrollback
    /// (docs/web-bind.md). The credential routes answer only on the Unix socket:
    /// possession of the host machine is the enrollment authority.
    Open {
        /// Print the link without launching a browser.
        #[arg(long)]
        print: bool,
        /// Show the authenticator-app enrollment QR (`otpauth://`) instead of a link.
        #[arg(long, conflicts_with = "bearer")]
        qr: bool,
        /// Print the program bearer token instead of a link.
        #[arg(long)]
        bearer: bool,
    },
    /// Drive a running session: the twelve control-API verbs (docs/operating.md)
    /// as one tool each. The same tools `efference mcp` serves over MCP, so a
    /// script and a mounting harness reach a session through one definition.
    #[command(subcommand)]
    Control(control::Tools),
    /// Serve the `control` tools as an MCP server on stdin/stdout
    /// (docs/operating.md) — the mount another harness supervises a session
    /// through. Speaks newline-delimited JSON-RPC and blocks until stdin closes.
    Mcp,
    /// Instruments for developing efference itself (docs/operating.md): the wire
    /// view, the element gallery, and the spotter/peek testers. Compiled only
    /// under the `dev` cargo feature, which is on by default.
    #[cfg(feature = "dev")]
    #[command(subcommand)]
    Dev(dev::DevCommand),
    /// Interactive client: tail the event stream and drive the daemon from one
    /// terminal. Type a message to send a turn; at an approval gate, Enter
    /// approves, `.abort` aborts, any other text redirects. Ctrl-D quits.
    Attach,
    /// Interactive TUI: a themed, scrolling, markdown-rendering view of the
    /// transcript, fed live from the daemon's event streams, with a composer and
    /// in-TUI approval gates. q quits, ? for help, t for the theme picker.
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
    /// Read a recorded session in the TUI, inert: the same transcript rendering as
    /// `tui`, over a session's `log.jsonl` instead of a live daemon. Nothing can be
    /// sent — no composer, no approvals, no loop verbs — and the log is read once,
    /// so a session that is still running shows as of the moment it was opened.
    /// q closes, ? for help, t for the theme picker.
    View {
        /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
        /// session directory, or a path to a `log.jsonl`. Omit to use `--name`;
        /// with neither, the available session names are listed.
        session: Option<String>,
        /// Syntax/UI theme — a built-in name or a `.tmTheme` path. Defaults to
        /// "Catppuccin Frappe".
        #[arg(long, env = "EFFERENCE_THEME")]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables.
        #[arg(long, env = "EFFERENCE_IMAGES", value_enum, default_value_t)]
        images: ImagesArg,
    },
    /// Report a session's prompt-cache read rate from its `usage.jsonl`
    /// (docs/log-and-projection.md) — the empirical half of the prefix oracle the
    /// byte pins cover statically. Offline: no daemon, no network. Reads warm/cold
    /// per `RequestOrigin`, separates the cold reads that need no explanation
    /// (session boot, a model change, an idle gap past the cache TTL) from the ones
    /// that do, and reports the longest run of the latter — a long run means the
    /// provider stopped matching a prefix the pins say did not change.
    Cache {
        /// Which session to read: a session name (`.efference/<name>/usage.jsonl`), a
        /// session directory, or a path to a `usage.jsonl`. Omit for the most
        /// recently written session.
        session: Option<String>,
        /// Sweep every session under `.efference/` instead of one — the corpus view,
        /// which is where a rate is worth comparing against.
        #[arg(long, conflicts_with = "session")]
        all: bool,
    },
    /// Validate a config template: parse it, apply the same checks `serve`
    /// applies at boot, resolve its `[context]` entries from the current
    /// directory (`exec` entries run), and mount its `[mcp.servers.*]` stanzas
    /// to enumerate each server's advertised tools against the file's `tools`
    /// grants. Prints what the file grants and how many preamble bytes it
    /// costs; exits non-zero with the boot-time error otherwise. No daemon —
    /// the terminus for setting a project up. Stored MCP credentials are
    /// reused; a server needing authorization is reported, never authorized
    /// here.
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

/// Mint the per-session scratch dir (docs/tools.md): `<system-temp>/efference-<rand>/`,
/// mkdtemp-style — a random name and create-or-retry (`create_dir` fails if the name
/// exists, so there is no check-then-create race), mode `0700` so it is private on a
/// shared-`/tmp` system (macOS `$TMPDIR` is already per-user). `std::env::temp_dir()`
/// rather than a literal `/tmp`, which on macOS is a symlink and not the real temp dir.
pub(crate) fn mint_scratch_dir() -> std::io::Result<PathBuf> {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let base = std::env::temp_dir();
    for _ in 0..16 {
        // `RandomState` seeds from OS entropy — an unpredictable-enough name;
        // create-or-retry (not check-then-create) is what closes the race.
        let n = RandomState::new().build_hasher().finish();
        let candidate = base.join(format!("efference-{n:016x}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "could not mint a scratch dir (16 name collisions)",
    ))
}
