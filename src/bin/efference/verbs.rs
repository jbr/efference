//! One thin control-channel client per CLI verb (docs/control-api.md).

use super::*;

/// Reconstruct and print the wire request for a recorded session JSONL — the
/// deterministic projection the assistant sees (docs/README.md, docs/log-and-projection.md). No daemon, no
/// socket, no network: load the log, fold it to `messages`, place the trailing
/// cache breakpoint, and rebuild the `system`/`tools` envelope exactly as a real
/// turn would (`tool_choice: auto`, docs/transport.md). `system` and `tools[]` are not in
/// the log; they're rebuilt from the current code + cwd, so the note on stderr
/// warns when the session may have run in a different environment.
pub(crate) async fn wire(
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
        // No unmounted-servers section: this offline view mounts no connected
        // providers at all (see the note below), so it has nothing to report
        // about which of them failed.
        efference::env::system_prompt(context_block.as_deref(), role_section.as_deref(), None)
    };
    // The full tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the docs/supervisor.md and docs/inbox.md tools) — so the dev view matches.
    // (No scratch dir: the wire view renders, it never executes.) One known gap:
    // config-declared connected providers (docs/tools.md) are not mounted here —
    // mounting spawns processes / opens connections, which an offline view must
    // not — so a session with `[mcp.servers.*]` has more tools[] than this shows.
    let toolbox = Toolbox::bash_and_mcps(cwd, None)?;
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

/// Read a recorded session in the TUI, inert (docs/tui.md): resolve the session to
/// a `log.jsonl`, load it with the same `persist::load` a live attach's entries come
/// through, and hand the entries to the read-only TUI. No socket, no network, no
/// writes — and no config, so it works on a session whose config no longer parses or
/// that ran in another directory.
pub(crate) async fn view(
    session: Option<String>,
    name: Option<String>,
    theme: Option<String>,
) -> CliResult {
    let Some(target) = session.or(name) else {
        return Err(format!(
            "which session? pass a session name, a session dir, or a log.jsonl path.{}",
            session_listing()
        )
        .into());
    };
    let (path, label) = resolve_session_log(&target)?;
    let entries = efference::persist::load(&path)?;
    if entries.is_empty() {
        return Err(format!("{} holds no entries", path.display()).into());
    }
    efference::tui::run_view(entries, label, theme).await?;
    Ok(())
}

/// Launch the TUI on the fixed element gallery — no daemon, no socket. With
/// `stream`, a scripted emitter plays live tool-call events over it on a timer.
pub(crate) async fn gallery(
    theme: Option<String>,
    stream: bool,
    chunk: usize,
    interval: u64,
) -> CliResult {
    let script = stream.then(|| efference::tui::StreamScript {
        chunk,
        interval: std::time::Duration::from_millis(interval),
    });
    efference::tui::run_gallery(theme, script).await?;
    Ok(())
}

/// Launch the interactive TUI against the daemon control socket. All rendering
/// lives in `efference::tui`; here we just hand it a control client and the
/// optional theme override.
pub(crate) async fn tui(socket: PathBuf, theme: Option<String>) -> CliResult {
    efference::tui::run(ControlClient::new(socket), theme).await?;
    Ok(())
}

pub(crate) async fn turn(socket: PathBuf, text: String, wait: bool) -> CliResult {
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

pub(crate) async fn approve(
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
pub(crate) async fn settle(socket: PathBuf) -> CliResult {
    println!("{}", ControlClient::new(socket).settle().await?);
    Ok(())
}

/// Read the daemon's one pending approval out of `GET /state`.
pub(crate) async fn pending_call_id(
    client: &ControlClient,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let state: serde_json::Value = serde_json::from_str(&client.state().await?)?;
    state
        .get("gate")
        .and_then(|p| p.get("call_id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .ok_or_else(|| "no pending approval; pass a call id explicitly".into())
}

pub(crate) async fn pace(
    socket: PathBuf,
    speed_ms: Option<u64>,
    hold: bool,
    resume: bool,
) -> CliResult {
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

pub(crate) async fn review_level(socket: PathBuf, level: String) -> CliResult {
    let set = ControlClient::new(socket).review_level(&level).await?;
    println!("review level: {set}");
    Ok(())
}

pub(crate) async fn interrupt(socket: PathBuf) -> CliResult {
    ControlClient::new(socket).interrupt().await?;
    println!("interrupted");
    Ok(())
}

pub(crate) async fn background(socket: PathBuf, note: Option<String>) -> CliResult {
    match ControlClient::new(socket)
        .background(note.as_deref())
        .await?
    {
        Some(id) => {
            println!("requested backgrounding p#{id} (lands only while it is still running)")
        }
        None => println!("no foreground tool running; nothing to background"),
    }
    Ok(())
}

pub(crate) async fn notify(
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

pub(crate) async fn dismiss(socket: PathBuf, id: u64) -> CliResult {
    ControlClient::new(socket).dismiss_notification(id).await?;
    println!("dismissed {id}");
    Ok(())
}

pub(crate) async fn state(socket: PathBuf) -> CliResult {
    let body = ControlClient::new(socket).state().await?;
    println!("{}", pretty(&body));
    Ok(())
}

pub(crate) async fn log(socket: PathBuf, since: Option<u64>) -> CliResult {
    let body = ControlClient::new(socket).log(since).await?;
    println!("{}", pretty(&body));
    Ok(())
}

/// Re-emit a JSON body pretty-printed; falls back to the raw string if it isn't
/// JSON (so an error body still prints legibly).
pub(crate) fn pretty(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string()))
        .unwrap_or_else(|| body.to_string())
}
