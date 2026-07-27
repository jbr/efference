//! Booting the daemon: bind the socket, spawn the watchers, and run the turn
//! loop (docs/control-channel.md).

use super::*;

/// Run the daemon: bind the control socket, spawn the HTTP server, and drive the
/// agent loop with a [`RemoteFrontend`]. The loop blocks until a `POST /turn`
/// arrives, so an idle daemon simply waits — retiring argv-seeding and the
/// "backgrounded run can't get its opening prompt" problem (docs/README.md).
pub async fn serve<D: Distiller, S: Spotter>(
    client: Client,
    toolbox: Toolbox,
    distiller: D,
    spotter: S,
    log: Log,
    socket: PathBuf,
    role: LoadedConfig,
    autonomous: bool,
) -> Result<(), Error> {
    let (shared, turns) = Shared::with_role_and_scratch(
        log,
        role,
        toolbox.scratch_dir().map(std::path::Path::to_path_buf),
    );
    // Set before the loop spawns: the gate reads it on the loop's thread (backlog/permissions/
    // session-dir-and-config-as-reviewed-file.md — the autonomous flip).
    shared.set_autonomous(autonomous);
    // Share the client between the loop and the keepalive task. `tools` is computed
    // once here for the ping; `agent::run` recomputes the same (byte-stable, docs/log-and-projection.md)
    // definitions internally, so both send an identical prefix.
    let client = Arc::new(client);
    // The exact tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the supervisor/inbox tools, docs/supervisor.md, docs/inbox.md). Built from the one source
    // (`agent::tool_definitions`) so the keepalive ping warms the *same* cache prefix
    // a real turn reads; any drift here silently breaks idle cache-warming (docs/log-and-projection.md).
    let tools = agent::tool_definitions(&toolbox);
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns,
    };

    // `with_shared_state` carries the Arc into every conn's TypeSet; `listeners()`
    // upgrades to the multi-listener builder (carrying that state over) so we can
    // `bind_uds`. The same builder's `bind_tcp`/`bind_env` is the deferred-TCP
    // path (docs/transport.md) — a config switch, not an abstraction to build.
    let server = trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(router());
    eprintln!("control socket: {}", socket.display());

    // The cache keepalive runs alongside the loop (docs/log-and-projection.md) — a transport-side
    // timer that never touches the log, so it composes with the loop without
    // sharing anything but the read-only client and the telemetry clocks.
    tokio::spawn(keepalive_loop(
        Arc::clone(&client),
        Arc::clone(&shared),
        tools,
    ));

    // The fs-watch initiator (docs/permissions.md): if the config resolved
    // from a file, watch it so external edits take effect live as committed `ConfigEdit`s. The
    // built-in default has no file to watch.
    if let Some(config_path) = shared.config_path().map(|p| p.to_path_buf()) {
        tokio::spawn(config_watch_loop(Arc::clone(&shared), config_path));
    }

    // Trillium owns the signal registry: by default it registers SIGINT/SIGTERM and
    // drives graceful shutdown through its `Swansong`. But the future `serve` awaits
    // is `agent::run`, which parks in `read_user_turn` on the turn channel — whose
    // senders live in the still-alive `Arc<Shared>`, so a C-c that stops the HTTP
    // listener would otherwise leave the loop parked forever (the `kill -9` hang).
    // Hook the *same* Swansong via `interrupt`: it drives the loop normally until
    // shutdown is initiated, then cancels it and resolves to `None`. (`server` is
    // held — not dropped — for the whole await so the listener stays up; it drops on
    // return, after shutdown has already begun.)
    // Keep a handle for teardown — `run` moves the loop's `Arc<Shared>`.
    let teardown = Arc::clone(&shared);
    let run = agent::run(&client, &mut frontend, shared, distiller, spotter, toolbox);
    let outcome = match server.swansong().interrupt(run).await {
        Some(result) => result,
        None => {
            eprintln!("\nshutting down");
            Ok(())
        }
    };
    // Reap any background children before exiting so a killed daemon doesn't orphan
    // a process group (docs/supervisor.md, ownership).
    teardown.kill_all_bg();
    outcome
}
