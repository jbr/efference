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

/// Written into a fresh session's `config.toml` when there's no origin to snapshot — a header that
/// teaches the schema, plus the one default that isn't empty: an `Environment at boot` `[context]`
/// exec entry, orientation expressed as config rather than hardcoded (docs/context.md). It's a real,
/// visible line in the session's own file, so a session that `cat`s its config sees it and an
/// operator can edit or delete it (backlog/permissions/session-dir-and-config-as-reviewed-file.md).
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
#   [context]                           # knowledge the session boots with (docs/context.md), e.g.
#                                       #   \"CLAUDE.md\" = true or Label = { exec = \"...\" } (stdout at boot)

# Orientation as config: this section's stdout is frozen into the session preamble at boot.
[context]
\"Environment at boot\" = { exec = 'echo \"working directory: $PWD\"; echo \"platform: $(uname -sm)\"; echo \"top-level entries:\"; ls' }
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
    /// Detach the running foreground tool into the background supervisor
    /// (`POST /background`, docs/supervisor.md) — it keeps running as a `bg#<n>`
    /// process instead of blocking the turn. No-op if nothing is mid-tool.
    Background {
        /// A note delivered to the model as the next user message alongside the handoff.
        #[arg(long)]
        note: Option<String>,
    },
    /// Set the operator's pace dial / hold overlay (`POST /pace`,
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
    /// Set the review-level dial (`POST /review-level`, docs/permissions.md):
    /// who reviews a call the fast-path didn't clear.
    ReviewLevel {
        /// `autonomous` (spotter redirects the model), `spotter_human` (spotter → human gate), or
        /// `human_only` (skip the spotter; every gated call goes straight to the human).
        level: String,
    },
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
}

/// A spotter [`Stance`](efference::log::Stance) as a `--stance` filter value for the
/// `spotter-log` dev survey. Mirrors the log's four verdicts.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum StanceArg {
    Clear,
    ClearAlways,
    Watch,
    Gate,
}

impl StanceArg {
    fn matches(self, stance: efference::log::Stance) -> bool {
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
        Command::Background { note } => background(client_socket()?, note).await,
        Command::Pace {
            speed_ms,
            hold,
            resume,
        } => pace(client_socket()?, speed_ms, hold, resume).await,
        Command::ReviewLevel { level } => review_level(client_socket()?, level).await,
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
        Command::SpotterLog {
            path,
            stances,
            summary,
            list,
            no_fallback,
        } => spotter_log(path, stances, summary, list, no_fallback),
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
/// deterministic projection the assistant sees (docs/README.md, docs/log-and-projection.md). No daemon, no
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
    // (docs/context.md) — prefer it (the last one wins on a multi-boot log) so
    // the view shows exactly what the model saw, not a re-resolve of files that may have
    // drifted on disk since. Only logs that predate the snapshot fall back to a rebuild.
    let recorded_system = entries.iter().rev().find_map(|e| match &e.payload {
        efference::log::Payload::Preamble { system, .. } => Some(system.clone()),
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
        efference::env::system_prompt(context_block.as_deref(), role_section.as_deref())
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

async fn pace(socket: PathBuf, speed_ms: Option<u64>, hold: bool, resume: bool) -> CliResult {
    // `--hold` / `--resume` map to the `held` overlay; absent, only the speed dial (if given)
    // moves. `conflicts_with` already rejects `--hold --resume`.
    let held = match (hold, resume) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let (speed, held) = ControlClient::new(socket).pace(speed_ms, held).await?;
    println!(
        "pace: speed {speed}ms, {}",
        if held { "HELD" } else { "running" }
    );
    Ok(())
}

async fn review_level(socket: PathBuf, level: String) -> CliResult {
    let set = ControlClient::new(socket).review_level(&level).await?;
    println!("review level: {set}");
    Ok(())
}

async fn interrupt(socket: PathBuf) -> CliResult {
    ControlClient::new(socket).interrupt().await?;
    println!("interrupted");
    Ok(())
}

async fn background(socket: PathBuf, note: Option<String>) -> CliResult {
    ControlClient::new(socket).background(note.as_deref()).await?;
    println!("detach requested (lands only if a foreground tool is running)");
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

    // Provider resolution (docs/transport.md): endpoint URL + auth header, resolved once from
    // the environment. Defaults to Anthropic when only ANTHROPIC_API_KEY is set; point
    // EFFERENCE_ENDPOINT + EFFERENCE_API_KEY (raw secret) + EFFERENCE_AUTH_HEADER /
    // EFFERENCE_AUTH_PREFIX at OpenRouter's /api/v1/messages (or any Anthropic-Messages-
    // compatible endpoint) to switch, no code change. Secret kept alone so it can be a
    // whole-value `op://…` reference under `op run`.
    let endpoint = efference::api::Endpoint::from_env()?;

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
    let mut loaded = match efference::config::load(Some(&session_config)) {
        Ok(l) => l,
        Err(e) => {
            e.eprint();
            return Err("could not load session config".into());
        }
    };

    // Layer the harness's self-surface protection over the operator's grants (docs/permissions.md).
    // The session dir holds this session's control socket, its append-only log, and — under the same
    // base — every *other* session's dir; all of it sits inside the operator's `.` grant, so nothing
    // stops the session from writing its own log (corrupting the log) or POSTing to its own
    // control socket (approving its own gates, injecting a user turn) without this. Deny the whole
    // base dir, then re-allow read-only on *this* session's `config.toml` — the sanctioned
    // scope-expansion path: the session reads its own perimeter and *edits* the file, which is a
    // write the fast-path won't clear, so it gates for the operator. Inserted after the operator
    // config so an equal-specificity operator key is overridden; `policy::path_granted`'s
    // longest-prefix rule still lets a strictly deeper operator grant win.
    loaded
        .role
        .paths
        .insert(BASE_DIR.to_string(), efference::config::Perms::default());
    loaded.role.paths.insert(
        session_config.to_string_lossy().into_owned(),
        efference::config::Perms {
            read: true,
            write: false,
        },
    );

    let log_path = session_dir.join(LOG_NAME);
    let mut log = Log::create(&log_path)?;
    eprintln!("session dir: {}", session_dir.display());
    eprintln!("logging to {}", log_path.display());
    let socket = session_dir.join(SOCKET_NAME);
    let _ = std::fs::remove_file(&socket); // ignore if absent.

    // Build the system prompt once, here at startup, and freeze it into the cached
    // prefix — it must stay byte-stable for the whole session or it would invalidate
    // the cache every turn (docs/log-and-projection.md). Everything it folds in is resolved
    // once at boot: the [context] block (files read + exec sections run now, frozen —
    // docs/context.md; orientation rides here as a config exec entry), the role's
    // brief, and the config note.
    let cwd = std::env::current_dir()?;
    let role_section = loaded.role.preamble_section();
    // The [context] preamble builder (docs/context.md): resolve the operator's listed
    // files from cwd and fold the rendered block into the frozen prefix. Fail closed — configured
    // context that can't be read aborts startup rather than silently vanishing.
    let resolved = match efference::context::resolve(&loaded.role.context, &cwd) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("error: {e}");
            return Err("could not resolve [context] entries".into());
        }
    };
    let context_block = resolved.render();
    let system = efference::env::system_prompt(context_block.as_deref(), role_section.as_deref());
    // Snapshot the rendered boot preamble into the log (docs/context.md): the
    // [context] block folds in file contents that drift on disk, so the preamble is no longer
    // reconstructable from config + current files — this one entry keeps the log the complete
    // record of the wire. `efference wire` reads it back instead of rebuilding.
    log.append(
        0,
        efference::log::Payload::Preamble {
            system: system.clone(),
            context: resolved,
        },
    )?;

    let client = Client::new(endpoint.clone(), model, system);

    // The independent permission rung (docs/permissions.md): a fresh-context spotter
    // on its own Haiku-band client, built from the same endpoint. It re-decides the calls
    // the deterministic fast-path can't clear, before the human gate.
    let spotter = ForkSpot::new(endpoint, spotter_model);

    // The model-driven fork (docs/redaction-fork.md): every tool result is peeked and the
    // canonical wire keeps only what the model retains. `Toolbox` is the docs/tools.md tool
    // layer; mount bash + the in-process MCP servers (fs-mcp + semantic-edit-mcp),
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
    let endpoint = efference::api::Endpoint::from_env()?;
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
    let spotting = ForkSpot::new(endpoint, model)
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

// ---- spotter-log survey (docs/permissions.md) ----
// Minimal ANSI so the survey reads at a glance on a tty and stays clean when piped.
const BOLD: &str = "1";
const DIM: &str = "2";
const RED: &str = "31";
const GREEN: &str = "32";
const YELLOW: &str = "33";
const GREEN_BOLD: &str = "1;32";

/// Wrap `s` in an ANSI SGR code, but only when writing to a terminal — piped output stays plain
/// (and grep/`jq`-friendly). The reset always closes the span so padded widths stay correct.
fn paint(s: &str, code: &str, tty: bool) -> String {
    if tty {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// The uppercase display label and colour for a stance in the survey listing.
fn stance_label(stance: efference::log::Stance) -> (&'static str, &'static str) {
    use efference::log::Stance;
    match stance {
        Stance::Clear => ("CLEAR", GREEN),
        Stance::ClearAlways => ("CLEAR_ALWAYS", GREEN_BOLD),
        Stance::Watch => ("WATCH", YELLOW),
        Stance::Gate => ("GATE", RED),
    }
}

/// A compact one-line description of the tool call a verdict judged, for the survey listing.
/// Prefers the human-obvious field per tool shape — a bash `command`, else a `path`/`file_path`,
/// else a `pattern`/`query` — falling back to compact JSON of the whole input.
fn describe_call(name: &str, input: &serde_json::Value) -> String {
    let field = |k: &str| input.get(k).and_then(|v| v.as_str());
    // A `paths` array (the structured read/list tools) → its string elements, comma-joined.
    let paths = input.get("paths").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    if let Some(cmd) = field("command") {
        format!("{name}: {}", one_line(cmd))
    } else if let Some(p) = field("path").or_else(|| field("file_path")) {
        format!("{name}: {p}")
    } else if let Some(p) = paths.filter(|s| !s.is_empty()) {
        format!("{name}: {p}")
    } else if let Some(q) = field("pattern").or_else(|| field("query")) {
        format!("{name}: {q}")
    } else {
        format!("{name}: {}", one_line(&input.to_string()))
    }
}

/// Collapse whitespace/newlines to single spaces and cap length so a call reads as one tidy cell.
fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 120;
    if flat.chars().count() > MAX {
        let head: String = flat.chars().take(MAX - 1).collect();
        format!("{head}…")
    } else {
        flat
    }
}

/// One surveyed spotter verdict, flattened for the `spotter-log` listing.
struct SpotRow {
    session: String,
    seq: u64,
    turn: u32,
    stance: efference::log::Stance,
    origin: efference::log::Origin,
    reason: String,
    probed_prefix: Option<String>,
    call: String,
}

/// Extract the spotter verdicts from one session's loaded log, in append order, correlating each
/// `Payload::Spot` to the tool call it judged (by `call_id`) and applying the `--stance` /
/// `--no-fallback` filters. The pure, testable core of [`spotter_log`].
fn spots_in(
    session: &str,
    entries: &[efference::log::LogEntry],
    stances: &[StanceArg],
    no_fallback: bool,
) -> Vec<SpotRow> {
    use efference::log::{CallId, Origin, Payload, ToolCall};
    use std::collections::HashMap;

    // Index every tool call by id so each Spot can name what it judged.
    let mut calls: HashMap<&CallId, &ToolCall> = HashMap::new();
    for entry in entries {
        if let Payload::Assistant { calls: cs, .. } = &entry.payload {
            for c in cs {
                calls.insert(&c.id, c);
            }
        }
    }

    let mut rows = Vec::new();
    for entry in entries {
        let Payload::Spot(spot) = &entry.payload else {
            continue;
        };
        if no_fallback && spot.origin == Origin::Fallback {
            continue;
        }
        if !stances.is_empty() && !stances.iter().any(|s| s.matches(spot.stance)) {
            continue;
        }
        let call = calls
            .get(&spot.call_id)
            .map(|c| describe_call(&c.name, &c.input))
            .unwrap_or_else(|| format!("<unresolved call {}>", spot.call_id.0));
        rows.push(SpotRow {
            session: session.to_string(),
            seq: entry.seq,
            turn: entry.turn,
            stance: spot.stance,
            origin: spot.origin,
            reason: spot.reason.clone(),
            probed_prefix: spot.probed_prefix.clone(),
            call,
        });
    }
    rows
}

/// Resolve which session logs the survey reads. An explicit path is either a `log.jsonl` file
/// (used directly) or a session dir (→ its `LOG_NAME`); omitting it scans every
/// `.efference/*/log.jsonl` under the cwd. Returns `(session id, log path)` pairs, sorted by id
/// (the timestamp id sorts oldest→newest).
fn resolve_spotter_logs(path: Option<PathBuf>) -> std::io::Result<Vec<(String, PathBuf)>> {
    if let Some(p) = path {
        let (id, log) = if p.is_dir() {
            let id = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("session")
                .to_string();
            (id, p.join(LOG_NAME))
        } else {
            // A log file directly — name it by its parent dir (the session id) when we can.
            let id = p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("session")
                .to_string();
            (id, p)
        };
        return Ok(vec![(id, log)]);
    }
    let mut out = Vec::new();
    let base = Path::new(BASE_DIR);
    if !base.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let log = entry.path().join(LOG_NAME);
        if log.is_file() {
            out.push((entry.file_name().to_string_lossy().into_owned(), log));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Survey the recorded spotter verdicts across sessions (docs/permissions.md): read every
/// `Payload::Spot` back out of the session logs, correlate each to the tool call it judged, and
/// print a stance-distribution summary and/or the per-verdict listing. Offline — no daemon, no
/// network, config-independent. The read side of the `spotter` dev tool.
fn spotter_log(
    path: Option<PathBuf>,
    stances: Vec<StanceArg>,
    summary_only: bool,
    list_only: bool,
    no_fallback: bool,
) -> CliResult {
    use efference::log::{Origin, Stance};
    use std::io::IsTerminal;

    let logs = resolve_spotter_logs(path)?;
    if logs.is_empty() {
        eprintln!("no session logs found (looked under {BASE_DIR}/*/{LOG_NAME}).");
        return Ok(());
    }
    let tty = std::io::stdout().is_terminal();

    let mut rows: Vec<SpotRow> = Vec::new();
    let mut sessions_with_spots = 0usize;
    for (session, log_path) in &logs {
        let entries = match efference::persist::load(log_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skipping {}: {e}", log_path.display());
                continue;
            }
        };
        let found = spots_in(session, &entries, &stances, no_fallback);
        if !found.is_empty() {
            sessions_with_spots += 1;
        }
        rows.extend(found);
    }

    if rows.is_empty() {
        let suffix = if stances.is_empty() && !no_fallback {
            ""
        } else {
            " matching the filter"
        };
        println!("no spotter verdicts{suffix} across {} log(s).", logs.len());
        return Ok(());
    }

    // ---- summary: stance distribution + fail-closed rate ----
    if !list_only {
        let total = rows.len();
        let count = |s: Stance| rows.iter().filter(|r| r.stance == s).count();
        let fallback = rows.iter().filter(|r| r.origin == Origin::Fallback).count();
        let pct = |n: usize| n as f64 * 100.0 / total as f64;

        println!("{}", paint("SPOTTER SURVEY", BOLD, tty));
        println!(
            "  {} log(s) scanned · {sessions_with_spots} with verdicts · {total} verdict(s)",
            logs.len()
        );
        let line = |label: &str, n: usize, code: &str| {
            if n > 0 {
                let lbl = paint(&format!("{label:<12}"), code, tty);
                println!("  {lbl} {n:>4}  ({:>3.0}%)", pct(n));
            }
        };
        line("clear", count(Stance::Clear), GREEN);
        line("clear_always", count(Stance::ClearAlways), GREEN_BOLD);
        line("watch", count(Stance::Watch), YELLOW);
        line("gate", count(Stance::Gate), RED);
        if fallback > 0 {
            let lbl = paint(&format!("{:<12}", "fallback"), DIM, tty);
            println!(
                "  {lbl} {fallback:>4}  ({:>3.0}%)  fail-closed",
                pct(fallback)
            );
        }
    }

    // ---- listing: each verdict, grouped by session ----
    if !summary_only {
        if !list_only {
            println!();
        }
        let mut current = "";
        for r in &rows {
            if r.session.as_str() != current {
                current = r.session.as_str();
                let hdr = format!("── {} ", r.session);
                println!("{}", paint(&format!("{hdr:─<64}"), DIM, tty));
            }
            let (label, code) = stance_label(r.stance);
            let mut tags = String::new();
            if r.origin == Origin::Fallback {
                tags.push_str(" · fallback");
            }
            if let Some(p) = &r.probed_prefix {
                tags.push_str(&format!(" · probe `{p}`"));
            }
            println!(
                "  seq {} · turn {} · {}{tags}",
                r.seq,
                r.turn,
                paint(label, code, tty)
            );
            println!("    {}", r.call);
            println!("    {}", paint(&format!("“{}”", r.reason.trim()), DIM, tty));
        }
    }
    Ok(())
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
            efference::log::Payload::User(vec![efference::log::ContentBlock::Text("hello".into())]),
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
        assert!(
            full.is_err(),
            "expected the full envelope to reject the bad config"
        );

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
        assert!(
            lean.is_ok(),
            "expected --messages to succeed despite the bad config: {lean:?}"
        );

        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(&bad_config);
    }

    // ---- spotter-log survey ----

    fn entry(seq: u64, payload: efference::log::Payload) -> efference::log::LogEntry {
        efference::log::LogEntry {
            seq,
            turn: 1,
            at: efference::log::Timestamp(0),
            payload,
        }
    }

    /// A log with one bash call, a `Gate`(model) verdict on it, and a `Clear`(fallback)
    /// verdict on a call id that isn't in the log (the fail-closed-on-a-missing-call shape).
    fn spot_fixture() -> Vec<efference::log::LogEntry> {
        use efference::log::{CallId, Origin, Payload, Spot, Stance, ToolCall};
        vec![
            entry(
                1,
                Payload::Assistant {
                    thinking: vec![],
                    text: vec![],
                    calls: vec![ToolCall {
                        id: CallId("toolu_a".into()),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "rm  -rf\n ./build" }),
                    }],
                },
            ),
            entry(
                2,
                Payload::Spot(Spot {
                    call_id: CallId("toolu_a".into()),
                    stance: Stance::Gate,
                    reason: "deletes outside the sanctioned scope".into(),
                    origin: Origin::Model,
                    probed_prefix: None,
                }),
            ),
            entry(
                3,
                Payload::Spot(Spot {
                    call_id: CallId("toolu_gone".into()),
                    stance: Stance::Clear,
                    reason: "spotter unavailable".into(),
                    origin: Origin::Fallback,
                    probed_prefix: None,
                }),
            ),
        ]
    }

    #[test]
    fn spots_in_correlates_calls_and_flattens_whitespace() {
        use efference::log::Stance;
        let rows = spots_in("sess", &spot_fixture(), &[], false);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].stance, Stance::Gate);
        // The bash command is resolved from the Assistant call and collapsed to one line.
        assert_eq!(rows[0].call, "bash: rm -rf ./build");
        // A Spot whose call id isn't in the log still surfaces, marked unresolved.
        assert!(rows[1].call.contains("unresolved"));
        assert!(rows[1].call.contains("toolu_gone"));
    }

    #[test]
    fn spots_in_no_fallback_drops_fail_closed_verdicts() {
        use efference::log::Origin;
        let rows = spots_in("sess", &spot_fixture(), &[], true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].origin, Origin::Model);
    }

    #[test]
    fn spots_in_stance_filter_keeps_only_matching() {
        use efference::log::Stance;
        let gates = spots_in("sess", &spot_fixture(), &[StanceArg::Gate], false);
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].stance, Stance::Gate);
        // A stance nobody has → empty.
        assert!(spots_in("sess", &spot_fixture(), &[StanceArg::Watch], false).is_empty());
    }

    #[test]
    fn describe_call_prefers_the_obvious_field() {
        assert_eq!(
            describe_call("bash", &serde_json::json!({ "command": "ls -la" })),
            "bash: ls -la"
        );
        assert_eq!(
            describe_call("read", &serde_json::json!({ "file_path": "src/log.rs" })),
            "read: src/log.rs"
        );
        // The structured read/list tools carry a `paths` array — joined, not dumped as JSON.
        assert_eq!(
            describe_call("read", &serde_json::json!({ "paths": ["a.rs", "b.rs"] })),
            "read: a.rs, b.rs"
        );
        // No obvious key → compact JSON of the whole input, so the call is never opaque.
        let other = describe_call("mystery", &serde_json::json!({ "z": 1, "a": 2 }));
        assert!(other.starts_with("mystery: "));
        assert!(other.contains("\"a\""));
    }

    #[test]
    fn paint_is_plain_when_not_a_tty() {
        assert_eq!(paint("x", RED, false), "x");
        assert_eq!(paint("x", RED, true), "\x1b[31mx\x1b[0m");
    }
}
