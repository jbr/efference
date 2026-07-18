//! The binary — the control-channel daemon entry point.
//!
//! `efference serve` opens a JSONL session log, binds the control socket, and runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (docs/README.md, docs/control-channel.md):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turn`, not argv. Runs against Haiku to keep the blast radius
//! cheap; docs/transport.md locks `claude-opus-4-8` as the real default.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use efference::api::Client;
use efference::client::Client as ControlClient;
use efference::daemon;
use efference::distill::ForkPeek;
use efference::frontend::Approval;
use efference::persist::Log;
use efference::spotter::{ForkSpot, Spotter};
use efference::tools::Toolbox;
use futures_lite::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};

/// The base directory for all session dirs (gitignored). Each session is `.efference/<id>/`,
/// holding its standard-named socket, log, and config (backlog/permissions/session-dir-and-config-as-reviewed-file.md).
const BASE_DIR: &str = ".efference";
const SOCKET_NAME: &str = "control.sock";
const LOG_NAME: &str = "log.jsonl";
const CONFIG_NAME: &str = "config.toml";

/// Written into a fresh session's `config.toml` when there's no origin to snapshot — an empty config
/// (parses to the default) with a header that teaches the schema, so a session that `cat`s it sees
/// what it can edit (backlog/permissions/session-dir-and-config-as-reviewed-file.md).
const CONFIG_SCAFFOLD: &str = "\
# Your session's permissions config. Edit it to request changes — edits are reviewed before they
# take effect, and a malformed file is rejected wholesale (fail-closed): if an edit doesn't seem to
# \"take\", check that the file still parses. Unknown keys are rejected. Schema (all keys optional):
#   scope = \"...\"                       # STRING, top-level — what this session is for (NOT a [scope] table)
#   watch-for = [\"...\"]                 # hazards the permission spotter should watch for
#   trust-threshold = 0.5               # where the watch-window/gate cutoffs sit (optional float)
#   tools = [\"read\", \"list\", \"search\"]  # structured tools you may use
#   [commands]                          # bash prefixes that auto-clear, e.g. cargo = true or \"git diff\" = true
#   [paths]                             # filesystem grants, e.g. \".\" = true or \"/refs\" = { read = true }
";

#[derive(Parser)]
#[command(name = "efference", about = "An API-backed agentic coding harness.")]
struct Cli {
    /// Session name. `serve` creates `.efference/<name>/`; a client verb locates that session's
    /// socket. Omitted: `serve` generates a timestamp id; a client uses the single live daemon
    /// (discovered by scanning the session dirs), erroring if several are live.
    #[arg(long, global = true)]
    name: Option<String>,
    /// Explicit control-socket path — an escape hatch that overrides `--name` and discovery.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// The directory for a session id (`.efference/<id>`).
fn session_dir(id: &str) -> PathBuf {
    Path::new(BASE_DIR).join(id)
}

/// A default session id when `--name` is omitted: timestamp millis — sortable, human-meaningful,
/// and matching the old flat `session-<millis>` naming.
fn default_session_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis()
        .to_string()
}

/// Resolve the control socket a *client* verb should talk to: an explicit `--socket`, else the
/// named session's socket, else the single **live** daemon discovered by scanning the session dirs
/// (backlog/permissions/session-dir-and-config-as-reviewed-file.md).
fn resolve_client_socket(name: Option<&str>, socket: Option<PathBuf>) -> Result<PathBuf, CliError> {
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
fn discover_live_socket() -> Result<PathBuf, CliError> {
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

type CliError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: bind the control socket and drive the agent loop. Waits for
    /// the first user turn over the socket (`POST /turn`).
    Serve {
        /// Model to use for the agent. Defaults to claude-haiku-4-5.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Model for the spotter permission rung (docs/permissions.md). Independent
        /// of the agent model — the spotter is volume work, so it defaults to the
        /// cheap band (Haiku) regardless of what the agent runs.
        #[arg(long, default_value = "claude-haiku-4-5")]
        spotter_model: String,
        /// Path to the config (TOML, see backlog/permissions/README.md). If omitted,
        /// `efference.toml` in the cwd is probed; absent → the built-in default config.
        /// An explicit path that doesn't exist is an error. The resolved path is
        /// reported on `GET /state`.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Run **autonomous** (no synchronous human at the gate): a spotter `Watch`/`Gate`
        /// becomes a redirect that hands the call back to the model, pointing it at its config
        /// file to request access, instead of blocking on an absent human. A config-file write
        /// still gates — held for the orchestrator-above to review over the control channel
        /// (backlog/permissions/session-dir-and-config-as-reviewed-file.md).
        #[arg(long)]
        autonomous: bool,
    },
    /// Send a user turn to a running daemon (`POST /turn`). Seeds turn 1 on an
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
    /// idle — then print its `/state` (`GET /settle`). The wait-without-acting verb.
    Settle,
    /// Interrupt a running tool (`POST /interrupt`) — kills its process group.
    Interrupt,
    /// Post an ambient notification into the daemon's inbox (`POST /notify`, docs/inbox.md).
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
    },
    /// Dev tool (not part of the shippable product, like `gallery`): point it at a
    /// session JSONL log and print the wire request — the deterministic projection
    /// the assistant actually sees. Reconstructs the full envelope (`system` +
    /// `tools` + `messages`) exactly as a real turn POSTs it: `project(log)` with
    /// the trailing cache breakpoint placed and `tool_choice: auto`. The `system`
    /// block and `tools[]` aren't stored in the log, so they're rebuilt from the
    /// current code and cwd — flagged on stderr, since the recorded session may
    /// have run elsewhere.
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
    /// (docs/permissions.md) on a single (scope, watch-for, command) and print its
    /// verdict — no daemon, no session. A live API call to the spotter model, for
    /// iterating on the persona and config against real cases. Requires
    /// ANTHROPIC_API_KEY.
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
        /// arguments (backlog/permissions/prefix-promotion.md). The verdict's `clear_always` stance is
        /// the safe-vote; the tester reports whether it would queue a promotion.
        #[arg(long)]
        probe: Option<String>,
        /// The bash command to judge. Multiple words are joined with spaces.
        #[arg(required = true, num_args = 1..)]
        command: Vec<String>,
    },
}

/// Output format for the `wire` dev view.
#[derive(Clone, Copy, clap::ValueEnum)]
enum WireFormat {
    /// Pretty-printed JSON — true to the bytes POSTed; pipeable to `jq`.
    Json,
    /// Lossy, read-only pseudo-YAML that unfolds escaped/multi-line strings.
    Yaml,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    // Client verbs talk to a daemon over a socket resolved from `--name`/discovery; the daemon and
    // the offline verbs (spotter/gallery/wire) don't. Resolve lazily, per arm, so an offline verb
    // never triggers discovery.
    let name = cli.name.clone();
    let socket_override = cli.socket.clone();
    let client_socket = || resolve_client_socket(name.as_deref(), socket_override.clone());
    match cli.command {
        Command::Serve {
            model,
            spotter_model,
            config,
            autonomous,
        } => {
            // `serve` owns the session dir: `--name` or a generated timestamp id → `.efference/<id>/`,
            // with the socket/log/config colocated inside.
            let id = cli.name.unwrap_or_else(default_session_id);
            serve(session_dir(&id), &model, &spotter_model, config, autonomous).await
        }
        Command::Turn { text, wait } => turn(client_socket()?, text.join(" "), wait).await,
        Command::Approve {
            call_id,
            abort,
            redirect,
            wait,
        } => approve(client_socket()?, call_id, abort, redirect, wait).await,
        Command::Settle => settle(client_socket()?).await,
        Command::Interrupt => interrupt(client_socket()?).await,
        Command::Notify {
            source,
            body,
            slot,
            replaces,
        } => notify(client_socket()?, source, body.join(" "), slot, replaces).await,
        Command::Dismiss { id } => dismiss(client_socket()?, id).await,
        Command::State => state(client_socket()?).await,
        Command::Log { since } => log(client_socket()?, since).await,
        Command::Spotter {
            scope,
            watch_for,
            paths,
            model,
            probe,
            command,
        } => spotter_check(scope, watch_for, paths, &model, probe, command.join(" ")).await,
        Command::Attach => attach(client_socket()?).await,
        Command::Tui { theme } => tui(client_socket()?, theme).await,
        Command::Gallery { theme } => gallery(theme).await,
        Command::Wire {
            path,
            messages,
            format,
            model,
            config,
        } => wire(path, messages, format, &model, config).await,
    }
}

/// Reconstruct and print the wire request for a recorded session JSONL — the
/// deterministic projection the assistant sees (docs/README.md, docs/substrate.md). No daemon, no
/// socket, no network: load the log, fold it to `messages`, place the trailing
/// cache breakpoint, and rebuild the `system`/`tools` envelope exactly as a real
/// turn would (`tool_choice: auto`, docs/transport.md). `system` and `tools[]` are not in
/// the log; they're rebuilt from the current code + cwd, so the note on stderr
/// warns when the session may have run in a different environment.
async fn wire(
    path: PathBuf,
    messages_only: bool,
    format: WireFormat,
    model: &str,
    config: Option<PathBuf>,
) -> CliResult {
    let entries = efference::persist::load(&path)?;
    let mut messages = efference::project::project(&entries);
    efference::project::place_trailing_cache_breakpoint(&mut messages);

    // `--messages` prints just the projected array and returns before touching
    // config/system/tools at all — a lean, config-independent view of what the
    // model sees, so it still works when a session's config won't parse.
    if messages_only {
        eprintln!(
            "note: {} log entries → {} wire messages.",
            entries.len(),
            messages.len()
        );
        let rendered = match format {
            WireFormat::Json => efference::api::render_messages_pretty(&messages)?,
            WireFormat::Yaml => efference::api::render_messages_yaml(&messages)?,
        };
        println!("{rendered}");
        return Ok(());
    }

    // The `system` a session sent is recorded in the log as the boot preamble snapshot
    // (backlog/preamble-context.md) — prefer it (the last one wins on a multi-boot log) so
    // the view shows exactly what the model saw, not a re-resolve of files that may have
    // drifted on disk since. Only logs that predate the snapshot fall back to a rebuild.
    let recorded_system = entries.iter().rev().find_map(|e| match &e.payload {
        efference::log::Payload::Preamble { system } => Some(system.clone()),
        _ => None,
    });
    let cwd = std::env::current_dir()?;
    let system = if let Some(system) = recorded_system {
        eprintln!(
            "note: {} log entries → {} wire messages. `system` is the recorded boot \
             preamble; `tools[]` is rebuilt from the current code and cwd.",
            entries.len(),
            messages.len()
        );
        system
    } else {
        // No snapshot (a pre-snapshot log) — rebuild the envelope the same way `serve` does
        // (current cwd seeds both the env block and the MCP servers' session roots; the role
        // config folds its brief into the preamble).
        let loaded = match efference::config::load(config.as_deref()) {
            Ok(l) => l,
            Err(e) => {
                e.eprint();
                return Err("could not load role config".into());
            }
        };
        let role_section = loaded.role.preamble_section();
        // The dev view rebuilds the envelope from current code/cwd, not the recorded run, so it
        // passes the *resolved* config path (or none) — close enough for inspection; the note on
        // stderr warns.
        let config_path = match &loaded.source {
            efference::config::ConfigSource::Path(p) => Some(p.as_path()),
            efference::config::ConfigSource::Default => None,
        };
        // Best-effort for the dev view: if the [context] files don't resolve from the current cwd
        // (e.g. inspecting a session recorded elsewhere), note it and omit the block rather than fail.
        let context_block = match efference::context::resolve(&loaded.role.context, &cwd) {
            Ok(resolved) => resolved.render(),
            Err(e) => {
                eprintln!("note: [context] not resolved ({e}); omitting from the rebuilt envelope");
                None
            }
        };
        eprintln!(
            "note: {} log entries → {} wire messages. This log predates the boot-preamble \
             snapshot: `system`/`tools[]` are rebuilt from the current code and cwd, not the \
             recorded session.",
            entries.len(),
            messages.len()
        );
        efference::env::system_prompt(
            context_block.as_deref(),
            role_section.as_deref(),
            config_path,
        )
    };
    // The full tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the docs/supervisor.md and docs/inbox.md tools) — so the dev view matches.
    let toolbox = Toolbox::bash_and_mcps(cwd)?;
    let tools = efference::agent::tool_definitions(&toolbox);

    let choice = efference::api::ToolChoice::auto();
    let rendered = match format {
        WireFormat::Json => {
            efference::api::render_request_pretty(model, &system, &tools, choice, &messages)?
        }
        WireFormat::Yaml => {
            efference::api::render_request_yaml(model, &system, &tools, choice, &messages)?
        }
    };
    println!("{rendered}");
    Ok(())
}

/// Launch the TUI on the fixed element gallery — no daemon, no socket.
async fn gallery(theme: Option<String>) -> CliResult {
    efference::tui::run_gallery(theme).await?;
    Ok(())
}

/// Launch the interactive TUI against the daemon control socket. All rendering
/// lives in `efference::tui`; here we just hand it a control client and the
/// optional theme override.
async fn tui(socket: PathBuf, theme: Option<String>) -> CliResult {
    efference::tui::run(ControlClient::new(socket), theme).await?;
    Ok(())
}

type CliResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn turn(socket: PathBuf, text: String, wait: bool) -> CliResult {
    let client = ControlClient::new(socket);
    if wait {
        // Block until the turn settles, then print the resulting state (one synchronous step).
        println!("{}", client.turn_wait(&text).await?);
    } else {
        client.turn(&text).await?;
        println!("accepted");
    }
    Ok(())
}

async fn approve(
    socket: PathBuf,
    call_id: Option<String>,
    abort: bool,
    redirect: Option<String>,
    wait: bool,
) -> CliResult {
    let client = ControlClient::new(socket);
    // Default to the one pending gate (the loop is sequential, docs/README.md) so the
    // common case is a bare `efference approve`.
    let call_id = match call_id {
        Some(id) => id,
        None => pending_call_id(&client).await?,
    };
    let decision = match (abort, redirect) {
        (true, _) => Approval::Abort,
        (false, Some(message)) => Approval::Redirect(message),
        (false, None) => Approval::Approve,
    };
    if wait {
        // Resolve, then block until the daemon settles again — print where it landed.
        println!("{}", client.approve_wait(&call_id, &decision).await?);
    } else {
        client.approve(&call_id, &decision).await?;
        println!("resolved {call_id}");
    }
    Ok(())
}

/// Block until the daemon settles (a gate, or idle) and print its `/state` — the wait-only verb,
/// for watching where an in-flight turn lands without submitting anything.
async fn settle(socket: PathBuf) -> CliResult {
    println!("{}", ControlClient::new(socket).settle().await?);
    Ok(())
}

/// Read the daemon's one pending approval out of `GET /state`.
async fn pending_call_id(
    client: &ControlClient,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let state: serde_json::Value = serde_json::from_str(&client.state().await?)?;
    state
        .get("pending_approval")
        .and_then(|p| p.get("call_id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .ok_or_else(|| "no pending approval; pass a call id explicitly".into())
}

async fn interrupt(socket: PathBuf) -> CliResult {
    ControlClient::new(socket).interrupt().await?;
    println!("interrupted");
    Ok(())
}

async fn notify(
    socket: PathBuf,
    source: String,
    body: String,
    slot: Option<String>,
    replaces: Option<u64>,
) -> CliResult {
    let id = ControlClient::new(socket)
        .notify(&source, &body, slot.as_deref(), replaces)
        .await?;
    println!("notified (id {id})");
    Ok(())
}

async fn dismiss(socket: PathBuf, id: u64) -> CliResult {
    ControlClient::new(socket).dismiss_notification(id).await?;
    println!("dismissed {id}");
    Ok(())
}

async fn state(socket: PathBuf) -> CliResult {
    let body = ControlClient::new(socket).state().await?;
    println!("{}", pretty(&body));
    Ok(())
}

async fn log(socket: PathBuf, since: Option<u64>) -> CliResult {
    let body = ControlClient::new(socket).log(since).await?;
    println!("{}", pretty(&body));
    Ok(())
}

/// Re-emit a JSON body pretty-printed; falls back to the raw string if it isn't
/// JSON (so an error body still prints legibly).
fn pretty(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string()))
        .unwrap_or_else(|| body.to_string())
}

/// The `attach` REPL — a spartan interactive client (docs/README.md: authoritative-
/// but-spartan, the full TUI is a later client). A background task renders the
/// live event tail; the foreground reads stdin and, per line, either resolves the
/// pending approval gate or sends a new turn. Both halves are just clients of the
/// socket — no privileged path (docs/control-channel.md).
async fn attach(socket: PathBuf) -> CliResult {
    let client = ControlClient::new(socket);

    // Background: tail `/events` from the start of the session and render each
    // entry as it lands. Dropped when `attach` returns (process exit).
    // Background: render the durable event tail and the ephemeral delta stream in
    // ONE task. Merging them (rather than two racing tasks) lets the live-streamed
    // prose suppress its own committed echo: when a turn streamed text, the
    // committed entry's text blocks are omitted — only its tool-call/approval lines,
    // which are never streamed, still render. See `render_streams`.
    let tail = client.clone();
    let handle = tokio::spawn(async move {
        if let Err(e) = render_streams(tail).await {
            eprintln!("[render loop ended: {e}]");
        }
    });

    println!(
        "attached. type a message to send a turn. at an approval gate: Enter approves, \
         '.abort' aborts, any other text redirects. Ctrl-D to quit."
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let text = line.trim().to_string();
        // Check the gate per line: the loop is sequential, so there is at most one
        // pending call (docs/README.md), and stdin is dual-use — approval when a gate is
        // open (mirroring the old interactive frontend's Enter-approves policy),
        // otherwise a new turn.
        match pending_call_id(&client).await.ok() {
            Some(call_id) => {
                let decision = match text.as_str() {
                    "" => Approval::Approve,
                    ".abort" => Approval::Abort,
                    other => Approval::Redirect(other.to_string()),
                };
                client.approve(&call_id, &decision).await?;
                println!("[gate {call_id} → {decision:?}]");
            }
            None if text.is_empty() => {}
            None => {
                client.turn(&text).await?;
                println!("[turn sent]");
            }
        }
    }

    handle.abort();
    Ok(())
}

/// What the merged renderer has already shown live, so a committed entry can skip
/// re-printing it. Both flags are set as fragments stream and cleared when the
/// corresponding committed entry arrives.
#[derive(Default)]
struct RenderState {
    /// A turn's assistant prose was streamed → omit its committed text blocks.
    streamed_assistant: bool,
    /// A tool's output was streamed → close that live region before the result.
    streamed_tool: bool,
}

/// Render the durable event tail (`GET /events`) and the ephemeral delta stream
/// (`GET /stream`) from a single task (docs/README.md). Deltas physically precede the
/// commit they belong to (the daemon publishes them during inference, then appends
/// the entry), so the live fragments print first and the committed entry simply
/// finalizes — without the duplicated block the two-task version produced.
async fn render_streams(client: ControlClient) -> CliResult {
    let mut events = client.events(Some(0)).await?;
    let mut deltas = client.stream().await?;
    let mut state = RenderState::default();
    let mut deltas_done = false;
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(event) => {
                    let event = event?;
                    render_event(event.event_type(), event.data(), &mut state);
                }
                None => break, // the durable tail ended (daemon down) → stop.
            },
            // Guarded so a finished delta stream (None forever) can't busy-loop the
            // select; the durable tail keeps rendering on its own after.
            delta = deltas.next(), if !deltas_done => match delta {
                Some(delta) => render_delta(delta?, &mut state),
                None => deltas_done = true,
            },
        }
    }
    Ok(())
}

/// Render one ephemeral delta: append its fragment to the current line and flush
/// (no newline, so prose / output accrete as they stream). The first fragment of a
/// region prints a label and trips the matching [`RenderState`] flag so the
/// committed entry knows to omit its echo.
fn render_delta(event: trillium_client::sse::Event, state: &mut RenderState) {
    let value: serde_json::Value = serde_json::from_str(event.data()).unwrap_or_default();
    match event.event_type() {
        Some("assistant_delta") => {
            if let Some(text) = value["text"].as_str() {
                if !state.streamed_assistant {
                    print!("\n◂ assistant: ");
                    state.streamed_assistant = true;
                }
                print_flush(text);
            }
        }
        Some("tool_output") => {
            if let Some(chunk) = value["chunk"].as_str() {
                if !state.streamed_tool {
                    print!("\n· output: ");
                    state.streamed_tool = true;
                }
                print_flush(chunk);
            }
        }
        _ => {}
    }
}

fn print_flush(s: &str) {
    print!("{s}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Render one SSE event (a serialized [`LogEntry`]) as a concise line. Navigates
/// the JSON loosely (via `Value`) so the CLI stays decoupled from the log's
/// internal shape. `state` carries what was already shown live, so a streamed turn
/// finalizes without re-printing its prose (the dedup the merged renderer exists
/// for).
fn render_event(kind: Option<&str>, data: &str, state: &mut RenderState) {
    let Ok(entry) = serde_json::from_str::<serde_json::Value>(data) else {
        println!("· {data}");
        return;
    };
    let seq = entry["seq"].as_u64().unwrap_or(0);
    let payload = &entry["payload"];
    match kind {
        Some("user") => {
            let text = payload["User"]
                .as_array()
                .and_then(|blocks| blocks.first())
                .and_then(|b| b["Text"].as_str())
                .unwrap_or("");
            println!("\n#{seq} ▸ user: {text}");
        }
        Some("assistant") => {
            let assistant = &payload["Assistant"];
            // If the prose was streamed live, just close the live line and skip the
            // text blocks — only the never-streamed tool-call lines still render.
            if state.streamed_assistant {
                println!();
                state.streamed_assistant = false;
            } else {
                for block in assistant["text"].as_array().into_iter().flatten() {
                    if let Some(s) = block["Text"].as_str()
                        && !s.is_empty()
                    {
                        println!("\n#{seq} ◂ assistant: {s}");
                    }
                }
            }
            for call in assistant["calls"].as_array().into_iter().flatten() {
                let name = call["name"].as_str().unwrap_or("?");
                let input = call["input"].to_string();
                println!(
                    "\n#{seq} ◂ wants {name}: {input}  (Enter approves · text redirects · .abort)"
                );
            }
        }
        Some("tool_result") => {
            // Close the live output region (if any) before the committed result —
            // which is the fork *distillate*, not a dup of the raw streamed output.
            if state.streamed_tool {
                println!();
                state.streamed_tool = false;
            }
            let result = &payload["ToolResult"];
            let tag = if result["raw"]["is_error"].as_bool().unwrap_or(false) {
                "error"
            } else {
                "result"
            };
            // The wire is `{"Distilled": {keep, note}}`, `"Raw"`, or `"Elided"`. A
            // distilled result is re-rendered from raw + the stored verdict
            // (docs/redaction-fork.md — the bytes aren't stored); Raw/Elided show the raw content.
            let raw_content = result["raw"]["content"].as_str().unwrap_or("");
            let shown = if let Some(d) = result["wire"].get("Distilled") {
                let keep: efference::log::Keep =
                    serde_json::from_value(d["keep"].clone()).unwrap_or(efference::log::Keep::All);
                efference::project::render_distilled(raw_content, &keep, d["note"].as_str())
            } else {
                raw_content.to_string()
            };
            println!("\n#{seq} · tool {tag}:\n{shown}");
        }
        _ => println!("\n#{seq} · {data}"),
    }
}

async fn serve(
    session_dir: PathBuf,
    model: &str,
    spotter_model: &str,
    config: Option<PathBuf>,
    autonomous: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Daemon logging: RUST_LOG controls verbosity (default: efference at info, deps
    // quiet). The transport (request sizes, full API error bodies), the agent loop
    // (capped tool results), and the distiller (fork-peek fallbacks) emit through `log`.
    // `try_init` so a second `serve` in-process (tests) doesn't panic.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,efference=info"),
    )
    .try_init();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set in the environment")?;

    // Everything for this session is colocated in its own dir (gitignored), standard-named since
    // the dir is the unique unit (backlog/permissions/session-dir-and-config-as-reviewed-file.md).
    // Create it first; a re-`serve` of an existing `--name` reuses it.
    std::fs::create_dir_all(&session_dir)?;

    // Snapshot the origin config into the session's own `config.toml` *once* (a fresh session). The
    // origin (`--config`/the `efference.toml` probe) is a template consumed at boot; thereafter the
    // per-session snapshot is the live, editable, watched policy — so two sessions sharing an origin
    // diverge freely, and the session can `cat` its config to see its perimeter and edit it to
    // request changes. On resume the snapshot already exists and is kept verbatim.
    let session_config = session_dir.join(CONFIG_NAME);
    if !session_config.exists() {
        // Validate the origin first (fail fast on a malformed template, before binding anything),
        // then copy it verbatim (comments intact) — or scaffold an empty config when there's none.
        let origin = match efference::config::load(config.as_deref()) {
            Ok(l) => l,
            Err(e) => {
                e.eprint();
                return Err("could not load role config".into());
            }
        };
        match &origin.source {
            efference::config::ConfigSource::Path(p) => {
                std::fs::copy(p, &session_config)?;
                eprintln!(
                    "config: snapshot of {} → {}",
                    p.display(),
                    session_config.display()
                );
            }
            efference::config::ConfigSource::Default => {
                std::fs::write(&session_config, CONFIG_SCAFFOLD)?;
                eprintln!("config: scaffolded {}", session_config.display());
            }
        }
    } else {
        eprintln!("config: reusing {}", session_config.display());
    }

    // The per-session snapshot *is* the live config now (a malformed hand-edit on resume fails fast
    // here). Everything downstream — the preamble, fs-watch, `/state`, persisted config-edits — keys
    // off this path, not the origin.
    let loaded = match efference::config::load(Some(&session_config)) {
        Ok(l) => l,
        Err(e) => {
            e.eprint();
            return Err("could not load session config".into());
        }
    };

    let log_path = session_dir.join(LOG_NAME);
    let mut log = Log::create(&log_path)?;
    eprintln!("session dir: {}", session_dir.display());
    eprintln!("logging to {}", log_path.display());
    let socket = session_dir.join(SOCKET_NAME);
    let _ = std::fs::remove_file(&socket); // ignore if absent.

    // Append the session environment block once, here at startup, and freeze it
    // into the system prompt. It must stay byte-stable for the whole session or it
    // would invalidate the cached prefix every turn (docs/substrate.md) — so it carries
    // only session-stable facts (cwd, platform, project type), never volatile state
    // like git status. See `env::system_prompt`. The `wire` dev view reconstructs
    // the same string. The role's brief (if any) folds in here, frozen for the session
    // exactly like the env block (backlog/permissions/README.md — the
    // config→preamble projection must be byte-stable).
    let cwd = std::env::current_dir()?;
    let role_section = loaded.role.preamble_section();
    // The [context] preamble builder (backlog/preamble-context.md): resolve the operator's listed
    // files from cwd and fold the rendered block into the frozen prefix. Fail closed — configured
    // context that can't be read aborts startup rather than silently vanishing.
    let context_block = match efference::context::resolve(&loaded.role.context, &cwd) {
        Ok(resolved) => resolved.render(),
        Err(e) => {
            eprintln!("error: {e}");
            return Err("could not resolve [context] entries".into());
        }
    };
    let system = efference::env::system_prompt(
        context_block.as_deref(),
        role_section.as_deref(),
        Some(session_config.as_path()),
    );
    // Snapshot the rendered boot preamble into the log (backlog/preamble-context.md): the
    // [context] block folds in file contents that drift on disk, so the preamble is no longer
    // reconstructable from config + current files — this one entry keeps the log the complete
    // record of the wire. `efference wire` reads it back instead of rebuilding.
    log.append(
        0,
        efference::log::Payload::Preamble {
            system: system.clone(),
        },
    )?;

    let client = Client::new(api_key.clone(), model, system);

    // The independent permission rung (docs/permissions.md): a fresh-context spotter
    // on its own Haiku-band client, built from the same key. It re-decides the calls
    // the deterministic fast-path can't clear, before the human gate.
    let spotter = ForkSpot::new(api_key, spotter_model);

    // The model-driven fork (docs/redaction-fork.md): every tool result is peeked and the
    // canonical wire keeps only what the model retains. `Toolbox` is the docs/tools.md tool
    // seam; mount bash + the in-process MCP servers (fs-mcp + semantic-edit-mcp),
    // their session cwd seeded to where we're running (resolved once above for [context]).
    let toolbox = Toolbox::bash_and_mcps(cwd)?;
    if autonomous {
        eprintln!(
            "autonomous: no synchronous gate — spotter Watch/Gate redirects the model to its \
             config file; config writes still gate for the orchestrator-above"
        );
    }
    daemon::serve(
        client, toolbox, ForkPeek, spotter, log, socket, loaded, autonomous,
    )
    .await
}

/// Dev tool: run the spotter on one `(scope, watch-for, paths, command)` and print
/// the verdict (docs/permissions.md). No daemon — it builds a throwaway config and a
/// bash [`ToolCall`] and calls the exact `assess` path a real gate would, so what it
/// prints is what the loop would see. For iterating on the persona and config.
///
/// [`ToolCall`]: efference::log::ToolCall
async fn spotter_check(
    scope: Option<String>,
    watch_for: Vec<String>,
    paths: Vec<String>,
    model: &str,
    probe: Option<String>,
    command: String,
) -> CliResult {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set in the environment")?;
    let role = efference::config::Config {
        scope,
        watch_for,
        paths: paths.iter().map(|s| parse_path_grant(s)).collect(),
        ..Default::default()
    };
    let call = efference::log::ToolCall {
        id: efference::log::CallId("toolu_spotter_check".into()),
        name: efference::bash::NAME.into(),
        input: serde_json::json!({ "command": command }),
    };
    let spotting = ForkSpot::new(api_key, model)
        .assess(&role, &call, probe.as_deref())
        .await;
    println!("stance: {:?}", spotting.read.stance);
    println!("reason: {}", spotting.read.reason);
    if let Some(prefix) = &probe {
        // The probe's answer is the stance: `clear_always` (from a real model verdict) is the safe-vote.
        let safe = spotting.origin == efference::log::Origin::Model
            && spotting.read.stance == efference::log::Stance::ClearAlways;
        let verdict = if !safe {
            "not a safe-vote (stance wasn't clear_always) — no promotion".to_string()
        } else if let Some(key) = efference::policy::promotable_command(&role, prefix) {
            format!("safe-vote → would queue `{key}` as a Proposed(Command) at K=1")
        } else {
            "safe-vote, but already covered by the allowlist — nothing to queue".to_string()
        };
        println!("probe `{prefix}`: {verdict}");
    }
    println!("origin: {:?}", spotting.origin);
    if let Some(u) = spotting.usage {
        println!(
            "usage:  in {} · out {} · cache read {} · cache write {}",
            u.input_tokens,
            u.output_tokens,
            u.cache_read_input_tokens,
            u.cache_creation_input_tokens
        );
    }
    Ok(())
}

/// Parse a `--path` grant spec for the spotter tester into a `[paths]` map entry: `<path>:rw`
/// (read+write), `<path>:r` (read), or a bare `<path>` (no access — the honest default). The
/// perm suffix is matched by the letters it contains.
fn parse_path_grant(spec: &str) -> (String, efference::config::Perms) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Socket resolution precedence (backlog/permissions/session-dir-and-config-as-reviewed-file.md):
    /// an explicit `--socket` wins over everything; a `--name` resolves to that session's colocated
    /// socket. (The no-name discovery branch scans the filesystem and is exercised live.)
    #[test]
    fn socket_override_beats_name() {
        let got = resolve_client_socket(Some("foo"), Some(PathBuf::from("/tmp/x.sock"))).unwrap();
        assert_eq!(got, PathBuf::from("/tmp/x.sock"));
    }

    #[test]
    fn name_resolves_to_its_colocated_socket() {
        let got = resolve_client_socket(Some("foo"), None).unwrap();
        assert_eq!(got, Path::new(".efference/foo/control.sock"));
    }

    #[test]
    fn session_dir_is_under_the_base() {
        assert_eq!(session_dir("abc"), Path::new(".efference/abc"));
    }

    /// A minimal session log fixture: one user turn, one assistant reply. Written
    /// via [`Log::create`]/[`Log::append`] (like a real session) so `persist::load`
    /// reads back exactly what `wire` would see.
    fn write_fixture_log(path: &Path) {
        let mut log = Log::create(path).unwrap();
        log.append(
            0,
            efference::log::Payload::User(vec![efference::log::ContentBlock::Text(
                "hello".into(),
            )]),
        )
        .unwrap();
        log.append(
            1,
            efference::log::Payload::Assistant {
                thinking: vec![],
                text: vec![efference::log::ContentBlock::Text("hi there".into())],
                calls: vec![],
            },
        )
        .unwrap();
    }

    /// `wire --messages` is the point of this flag: a lean, config-independent
    /// view of `project(log)` that works even when the session's config won't
    /// parse. Exercise both halves — the full envelope path *does* fail on a
    /// malformed config, while `--messages` skips config loading entirely and
    /// succeeds regardless (docs: the flag's purpose is exactly this).
    #[tokio::test]
    async fn wire_messages_skips_the_unparseable_config() {
        let log_path =
            std::env::temp_dir().join(format!("efference-wire-log-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&log_path);
        write_fixture_log(&log_path);

        let bad_config = std::env::temp_dir().join(format!(
            "efference-wire-bad-config-{}.toml",
            std::process::id()
        ));
        std::fs::write(&bad_config, "this is not valid toml [[[").unwrap();

        // Without `--messages`, the full envelope rebuild loads the config and
        // fails loudly on the malformed file.
        let full = wire(
            log_path.clone(),
            false,
            WireFormat::Json,
            "claude-haiku-4-5",
            Some(bad_config.clone()),
        )
        .await;
        assert!(full.is_err(), "expected the full envelope to reject the bad config");

        // With `--messages`, config is never touched, so the same bad config
        // doesn't matter — it prints just the projected messages array.
        let lean = wire(
            log_path.clone(),
            true,
            WireFormat::Json,
            "claude-haiku-4-5",
            Some(bad_config.clone()),
        )
        .await;
        assert!(lean.is_ok(), "expected --messages to succeed despite the bad config: {lean:?}");

        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(&bad_config);
    }
}
