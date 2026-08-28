//! The clap command tree, session-directory layout, and socket resolution.

use super::*;

/// The base directory for all session dirs (gitignored). Each session is `.efference/<id>/`,
/// holding its standard-named socket, log, and config (docs/permissions.md).
pub(crate) const BASE_DIR: &str = ".efference";

pub(crate) const SOCKET_NAME: &str = "control.sock";

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
#   [commands]                          # bash prefixes that auto-clear, e.g. cargo = true or \"git diff\" = true
#   [paths]                             # filesystem grants, e.g. \".\" = true or \"/refs\" = { read = true }
#   [context]                           # knowledge the session boots with (docs/context.md), e.g.
#                                       #   \"CLAUDE.md\" = true or Label = { exec = \"...\" } (stdout at boot)

# Orientation as config: this section's stdout is frozen into the session preamble at boot.
[context]
\"Environment at boot\" = { exec = 'echo \"working directory: $PWD\"; echo \"platform: $(uname -sm)\"; echo \"top-level entries:\"; ls' }
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
    /// Send a user turn to a running daemon (`POST /turns`). Seeds turn 1 on an
    /// idle daemon. Returns immediately — watch `state`/`log` for the response,
    /// or pass `--wait` to block until the turn reaches a gate or completes.
    Turn {
        /// The turn text. Multiple words are joined with spaces.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Block until the daemon settles (a gate, or the turn completes), then print the
        /// resulting `/state` — synchronous "send a turn, read what happened" with no poll loop.
        #[arg(long)]
        wait: bool,
    },
    /// Resolve the pending tool-approval gate (`POST /approvals/{id}`). With no
    /// flags, approves. The call id defaults to the one pending approval.
    Approve {
        /// The `tool_use` call id. Defaults to the daemon's current pending gate.
        call_id: Option<String>,
        /// Reject and unwind the whole turn instead of approving.
        #[arg(long, conflicts_with = "redirect")]
        abort: bool,
        /// Reject this call and steer the model with this message (kept as the next
        /// user turn) instead of approving.
        #[arg(long)]
        redirect: Option<String>,
        /// Block until the daemon settles (the next gate, or the turn completes), then print
        /// the resulting `/state` — drive a multi-gate turn as a clean approve→read loop.
        #[arg(long)]
        wait: bool,
    },
    /// Block until the daemon reaches a decision point — a gate is pending, or the loop is
    /// idle — then print its `/state` (`GET /state?wait=settled`). The wait-without-acting verb.
    Settle,
    /// Interrupt a running tool (`POST /loop/interrupt`) — kills its process group.
    Interrupt,
    /// Detach the running foreground tool into the background supervisor
    /// (`PATCH /processes/:id {background: true}`, docs/supervisor.md) — it keeps running as a `p#<n>`
    /// process instead of blocking the turn. No-op if nothing is mid-tool.
    Background {
        /// A note delivered to the model as the next user message alongside the handoff.
        #[arg(long)]
        note: Option<String>,
    },
    /// Set the operator's pace dial / hold overlay (`PATCH /loop {pace}`,
    /// docs/control-channel.md). Between-turns wall-clock pacing, transparent to the
    /// agent (never logged or sent to the model). Combine `--speed-ms` with `--hold`/`--resume`.
    Pace {
        /// Between-turns delay in milliseconds (0 = as fast as possible).
        #[arg(long)]
        speed_ms: Option<u64>,
        /// Pause at the next between-turns juncture; the speed dial is retained underneath.
        #[arg(long, conflicts_with = "resume")]
        hold: bool,
        /// Release a hold — resume at the retained speed.
        #[arg(long)]
        resume: bool,
    },
    /// Set the review-level dial (`PATCH /loop {review_level}`, docs/permissions.md):
    /// who reviews a call the fast-path didn't clear.
    ReviewLevel {
        /// `autonomous` (spotter redirects the model), `spotter_human` (spotter → human gate), or
        /// `human_only` (skip the spotter; every gated call goes straight to the human).
        level: String,
    },
    /// Post an ambient notification into the daemon's inbox (`POST /notifications`, docs/inbox.md).
    /// It waits for the next triage gate; watch `state` for the pending inbox.
    Notify {
        /// Where it came from (e.g. "test-watch", "operator", "agent:reviewer").
        source: String,
        /// The notification text. Multiple words are joined with spaces.
        #[arg(required = true, num_args = 1..)]
        body: Vec<String>,
        /// Bind to a logical slot — a later notify on the same slot supersedes this.
        #[arg(long)]
        slot: Option<String>,
        /// Supersede a specific pending notification by its id.
        #[arg(long)]
        replaces: Option<u64>,
    },
    /// Dismiss a pending notification by id (`POST /notifications/{id}/dismiss`, docs/inbox.md).
    Dismiss {
        /// The notification id (from `notify`'s output or `state`).
        id: u64,
    },
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
    /// Element gallery: launch the TUI on a fixed "one of each" transcript (rich
    /// markdown, a shell call, a colored diff, a collapsed long result, an error, a
    /// distilled result, a consolidation mark) — no daemon. A live design page for
    /// iterating on transcript rendering. q quits, ? for help, t for the theme picker.
    Gallery {
        /// Syntax/UI theme — a built-in name or a `.tmTheme` path. Defaults to
        /// "Catppuccin Frappe".
        #[arg(long, env = "EFFERENCE_THEME")]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables.
        #[arg(long, env = "EFFERENCE_IMAGES", value_enum, default_value_t)]
        images: ImagesArg,
        /// Also play a scripted live stream into the gallery on a timer: a long
        /// file write whose JSON input arrives fragment by fragment, then a command
        /// whose output streams. Drives the render behaviours that only exist at
        /// animation rate.
        #[arg(long)]
        stream: bool,
        /// Bytes per streamed tool-input fragment (with `--stream`).
        #[arg(long, default_value_t = 24, requires = "stream")]
        chunk: usize,
        /// Milliseconds between streamed fragments (with `--stream`). Below the
        /// 30 ms paint tick, fragments fold into one frame.
        #[arg(long, default_value_t = 16, requires = "stream")]
        interval: u64,
        /// Time each paint and print the distribution (p50/p90/p99/max, and how
        /// many frames ran over the 30ms tick) on exit. Development
        /// instrumentation for the streaming render.
        #[arg(long)]
        frame_cost: bool,
    },
    /// Dev tool (not part of the shippable product, like `gallery`): point it at a
    /// session JSONL log and print the wire request — the deterministic projection
    /// the assistant actually sees. Reconstructs the full envelope (`system` +
    /// `tools` + `messages`) exactly as a real turn POSTs it: `project(log)` with
    /// the trailing cache breakpoint placed and `tool_choice: auto`. The `system`
    /// block and `tools[]` aren't stored in the log, so they're rebuilt from the
    /// current code and cwd — flagged on stderr, since the recorded session may
    /// have run elsewhere.
    /// Dev tool (like `wire`/`spotter`): replay a recorded session's fork-gate peeks
    /// (docs/redaction-fork.md) and report **first-attempt** compliance — did the peek
    /// answer with a verdict, or reach for a work tool? The live gate retries, so a
    /// session's own record shows that a first attempt failed but can't score one ask
    /// against another. Each gate carries its own prefix at its own session depth, and
    /// replaying one is a single warm prefix read, so a whole session is an eval set.
    /// Sends real requests unless `--dry-run`.
    Peek {
        /// Path to a session dir or a `log.jsonl` whose gates to replay.
        path: PathBuf,
        /// Replay only the gate triaging this result `seq` (as `Triage.for_result`
        /// names it, and as the listing prints it). Omit to sweep every gate.
        #[arg(long)]
        seq: Option<u64>,
        /// Draws per gate. A peek is sampled, not deterministic, so one draw per gate
        /// resolves only large differences; repeats buy a per-gate rate instead of a
        /// coin flip.
        #[arg(long, default_value = "3")]
        repeat: usize,
        /// Sample this many gates instead of sweeping all of them — for pricing a new
        /// model before committing to a full run. Sampled **evenly across the session**,
        /// never the first N: first-attempt failures climb with session depth, so a
        /// prefix of the gates measures the easy end and reads as a better rate than the
        /// model earns.
        #[arg(long)]
        limit: Option<usize>,
        /// Print the peek's trailing message instead of sending it — what makes an
        /// instruction change reviewable without spending a request.
        #[arg(long)]
        dry_run: bool,
        /// Model to replay against. Match the session's own model or the numbers
        /// aren't comparable to its record.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
    },
    Wire {
        /// Path to a session JSONL log (e.g. `.efference/session-<millis>.jsonl`).
        path: PathBuf,
        /// Print only the projected `messages` array — `project(log)` with the
        /// trailing cache breakpoint placed — skipping the `system`/`tools[]`
        /// envelope entirely. A lean, config-independent view of what the model
        /// actually sees: unlike the full envelope, this needs no config, model
        /// id, or rebuilt tool list, so it works even when a session's config
        /// won't parse. Composes with `--format`.
        #[arg(long)]
        messages: bool,
        /// Rendering: `json` (true-to-bytes, pipeable to `jq`) or `yaml` (a lossy,
        /// read-only pseudo-YAML that unfolds escaped/multi-line strings for
        /// readability — not parseable back).
        #[arg(long, value_enum, default_value = "json")]
        format: WireFormat,
        /// Model id to stamp into the envelope. Matches the `serve` default; only
        /// cosmetic here (the request isn't sent).
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Config file to fold into the reconstructed preamble, same resolution as
        /// `serve` (defaults to probing `efference.toml`). The config file isn't recorded
        /// in the log, so — like `system`/`tools[]` — it's rebuilt from the current
        /// environment; pass `--config` to match the session that produced the log.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
    /// Print the daemon's ephemeral control state (`GET /state`).
    State,
    /// Print the durable append-only log (`GET /log?since=<seq>`).
    Log {
        /// Only entries with `seq >= since`.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Dev tool (like `wire`/`gallery`): run the spotter permission rung
    /// (docs/permissions.md) on a single move — a bash command, or a structured tool
    /// call with `--tool` — and print its verdict; no daemon, no session. A live API
    /// call to the spotter model, for iterating on the persona and config against real
    /// cases. With `--config`, the declared MCP servers are mounted first, so a
    /// `--tool` move is judged against the server's real tool definition (the same
    /// brief a session's spotter would see). Requires ANTHROPIC_API_KEY.
    Spotter {
        /// The scope (perimeter prose) to judge against. Omit to test the no-scope frame.
        #[arg(long)]
        scope: Option<String>,
        /// A spotter-only "thing to watch for". Repeatable.
        #[arg(long = "watch-for")]
        watch_for: Vec<String>,
        /// An allowed path grant, repeatable. Format: `<path>:rw` (read+write),
        /// `<path>:r` (read), or bare `<path>` (no access). E.g. `--path .:rw --path /refs:r`.
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Spotter model. Defaults to claude-haiku-4-5.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Simulate a prefix-promotion probe riding this call: ask whether `<prefix>` is safe for all
        /// arguments (docs/permissions.md). The verdict's `clear_always` stance is
        /// the safe-vote; the tester reports whether it would queue a promotion.
        #[arg(long)]
        probe: Option<String>,
        /// Judge a structured tool call instead of a bash command. Takes the tool name
        /// in either spelling (`ferritin.get` or `ferritin__get`); its input comes from
        /// `--input`. The tool must be one the toolbox mounts — a built-in, or a tool
        /// from a server declared in `--config` — because the point is to judge it
        /// against its *real* definition (docs/permissions.md).
        #[arg(long, conflicts_with = "command")]
        tool: Option<String>,
        /// The `--tool` call's input, as JSON. Defaults to no arguments.
        #[arg(long, default_value = "{}")]
        input: String,
        /// A config file to take the mission from, and — the reason this exists — to
        /// mount the `[mcp.servers.<name>]` declarations from, so `--tool` can name a
        /// real connected tool. `--scope`/`--watch-for`/`--path` override what it
        /// supplies. Omit to judge against the flags alone (no servers mounted).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Print the exact move block sent to the spotter before the verdict. Implied by
        /// `--tool`, where seeing the rendered brief is usually the point.
        #[arg(long = "show-move")]
        show_move: bool,
        /// Withhold the tool's brief, judging a `--tool` move on its name and input alone
        /// — what the spotter saw before briefs existed. The A/B: run the same call with
        /// and without, and the stance difference is the brief's effect.
        #[arg(long = "no-brief")]
        no_brief: bool,
        /// The bash command to judge. Multiple words are joined with spaces.
        #[arg(required_unless_present = "tool", num_args = 1..)]
        command: Vec<String>,
    },
    /// Dev tool (like `wire`/`spotter`): survey the recorded spotter verdicts
    /// (docs/permissions.md) across sessions — read every `Payload::Spot` back out
    /// of the session logs and print a health summary (stance distribution,
    /// fail-closed rate) followed by each verdict: the tool call it judged, its
    /// stance, and its one-line reason. Pure offline log read — no daemon, no
    /// network, config-independent (works even when a session's config won't
    /// parse). The companion to the live `spotter` tester: `spotter` runs one fresh
    /// check, `spotter-log` reviews the ones that already ran.
    SpotterLog {
        /// A session dir (`.efference/<id>`) or a `log.jsonl` to survey. Omit to
        /// scan every `.efference/*/log.jsonl` under the cwd — the cross-session view.
        path: Option<PathBuf>,
        /// Show only verdicts with this stance (repeatable): `clear`, `clear-always`,
        /// `watch`, or `gate`. Omit for all. E.g. `--stance gate --stance watch` to
        /// read just the calls the spotter didn't wave through.
        #[arg(long = "stance", value_enum)]
        stances: Vec<StanceArg>,
        /// Print only the aggregate summary (stance distribution + fail-closed rate),
        /// skipping the per-verdict listing.
        #[arg(long, conflicts_with = "list")]
        summary: bool,
        /// Print only the per-verdict listing, skipping the aggregate summary.
        #[arg(long, conflicts_with = "summary")]
        list: bool,
        /// Hide the fail-closed fallback verdicts (`origin: Fallback` — a failed
        /// spotter call that defaulted to the gate), leaving only real model
        /// verdicts. Excludes them from both the summary and the listing.
        #[arg(long = "no-fallback")]
        no_fallback: bool,
    },
    /// Dev tool: run the labeled spotter corpus against a model
    /// (backlog/spotter-eval.md). Each case's config + call renders through the
    /// production `assess` path; verdicts score against the case's `expected`
    /// on the binary the system acts on (approve vs route-to-human). Reports
    /// per-class rates (human-miss = safety, false-gate = friction), the flip
    /// rate across repeated runs, latency percentiles, token totals, and — on
    /// OpenRouter — the provider-billed cost per generation id. Per-attempt
    /// detail lands in a results jsonl for offline cross-tabs.
    SpotterEval {
        /// A corpus file (jsonl), repeatable. A case missing an inline
        /// `expected` is joined against `--labels` by id; still-unlabeled cases
        /// are skipped and counted.
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        /// A labels file (jsonl of `{id, expected, …}`) joined onto `--cases`
        /// rows that carry no inline label.
        #[arg(long)]
        labels: Option<PathBuf>,
        /// Spotter model under test.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Attempts per case — the spotter is sampled behavior, so per-case
        /// consistency (the flip rate) is its own quality axis.
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Results path. Defaults to `eval/spotter/results-<model>.jsonl`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Run only the first N cases (after `--filter`) — the smoke-test knob.
        #[arg(long)]
        limit: Option<usize>,
        /// Run only cases whose id contains this substring (e.g. `syn/` for the
        /// synthetics, a session id for one session's cases).
        #[arg(long)]
        filter: Option<String>,
        /// Concurrent in-flight requests.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// A replacement persona file — the ablation knob: measure a persona
        /// edit against the same corpus instead of by ad-hoc replays.
        #[arg(long)]
        persona: Option<PathBuf>,
        /// Skip the per-generation cost lookup.
        #[arg(long = "no-cost")]
        no_cost: bool,
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

/// A spotter [`Stance`](efference::log::Stance) as a `--stance` filter value for the
/// `spotter-log` dev survey. Mirrors the log's four verdicts.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum StanceArg {
    Clear,
    ClearAlways,
    Watch,
    Gate,
}

impl StanceArg {
    pub(crate) fn matches(self, stance: efference::log::Stance) -> bool {
        use efference::log::Stance;
        matches!(
            (self, stance),
            (StanceArg::Clear, Stance::Clear)
                | (StanceArg::ClearAlways, Stance::ClearAlways)
                | (StanceArg::Watch, Stance::Watch)
                | (StanceArg::Gate, Stance::Gate)
        )
    }
}

/// Output format for the `wire` dev view.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum WireFormat {
    /// Pretty-printed JSON — true to the bytes POSTed; pipeable to `jq`.
    Json,
    /// Lossy, read-only pseudo-YAML that unfolds escaped/multi-line strings.
    Yaml,
}

/// Parse a `--path` grant spec for the spotter tester into a `[paths]` map entry: `<path>:rw`
/// (read+write), `<path>:r` (read), or a bare `<path>` (no access — the honest default). The
/// perm suffix is matched by the letters it contains.
pub(crate) fn parse_path_grant(spec: &str) -> (String, efference::config::Perms) {
    match spec.rsplit_once(':') {
        Some((path, perms)) => (
            path.into(),
            efference::config::Perms {
                read: perms.contains('r'),
                write: perms.contains('w'),
            },
        ),
        None => (
            spec.into(),
            efference::config::Perms {
                read: false,
                write: false,
            },
        ),
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
