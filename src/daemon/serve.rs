//! Booting the daemon: bind the socket, spawn the watchers, and run the turn
//! loop (docs/control-channel.md).

use super::*;

/// Run the daemon: bind the control socket, spawn the HTTP server, and drive the
/// agent loop with a [`RemoteFrontend`]. The loop blocks until a `POST /turns`
/// arrives, so an idle daemon simply waits — retiring argv-seeding and the
/// "backgrounded run can't get its opening prompt" problem (docs/README.md).
#[allow(clippy::too_many_arguments)] // the daemon's full boot surface; one caller
pub async fn serve<D: Distiller, S: Spotter>(
    client: Client,
    toolbox: Toolbox,
    distiller: D,
    spotter: S,
    log: Log,
    socket: PathBuf,
    role: LoadedConfig,
    autonomous: bool,
    capabilities: crate::api::ModelCapabilities,
    bind: Option<tcp::TcpBind>,
) -> Result<(), Error> {
    let (shared, turns) = Shared::full(
        log,
        role,
        toolbox.scratch_dir().map(std::path::Path::to_path_buf),
        client.upstream_failure(),
        client.upstream_retry(),
        client.image_placement(),
    );

    // Set before the loop spawns: the gate reads it on the loop's thread (backlog/permissions/
    // session-dir-and-config-as-reviewed-file.md — the autonomous flip).
    shared.set_autonomous(autonomous);
    // The boot-fetched model capabilities (docs/transport.md) — the caller already
    // treated a failed lookup as fatal, so by here the value always exists.
    shared.set_capabilities(capabilities);
    // Session cost telemetry (docs/transport.md): armed only where the provider
    // has a per-request accounting surface (OpenRouter's generation endpoint).
    // Direct Anthropic has none, so the readout stays absent rather than modeled.
    if client.endpoint().provider == crate::api::Provider::OpenRouter {
        shared.enable_cost_tracking(client.endpoint().clone());
    }
    // The per-request usage record (docs/log-and-projection.md) lives beside the
    // canonical log in the session dir — the socket's parent by construction.
    if let Some(session_dir) = socket.parent() {
        shared.attach_usage_log(&session_dir.join("usage.jsonl"));
    }
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

    // The TCP bind's credential state (docs/web-bind.md), armed before either spawn:
    // the UDS router mounts the host-only credential routes (`/auth/link` & co.), and
    // the TCP listener mounts the gated stack — the split *is* the trust model, so it
    // lives in listener topology rather than per-conn inference.
    let web = bind.map(tcp::arm);
    let web_auth = web.as_ref().map(|(auth, _)| Arc::clone(auth));

    // `with_shared_state` carries the Arc into every conn's TypeSet; `listeners()`
    // upgrades to the multi-listener builder (carrying that state over) so we can
    // `bind_uds`.
    let server = trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn((trillium_head::head(), tcp::host_router(web_auth), router()));
    eprintln!("control socket: {}", socket.display());

    // The optional TCP listener beside the UDS (docs/web-bind.md): the same router
    // behind the hardening + auth gate, sharing the UDS server's swansong so both
    // stop as one. Held for the whole await like `server` below.
    let mut _tcp_server = None;
    if let Some((auth, listener)) = web {
        let open = listener.open;
        let addr = listener.addr.clone();
        let handle = tcp::spawn(
            listener,
            Arc::clone(&auth),
            server.swansong(),
            Arc::clone(&shared),
        )
        .map_err(|e| -> Error { format!("failed to bind {addr}: {e}").into() })?;
        let link = auth.mint_link();
        eprintln!("web: {} (bound {addr})", auth.public_base());
        eprintln!("  {link}");
        handle.info().await; // wait for server to come up before launching browser
        if open && webbrowser::open(&link).is_err() {
            eprintln!("[could not open a browser for {link}]");
        }
        _tcp_server = Some(handle);
    }

    // The one state-topic differ (docs/control-api.md): recomputes the topics on any
    // change signal and publishes the ones that moved, so N connected clients cost one
    // recomputation instead of N polls.
    tokio::spawn(watch_topics(Arc::clone(&shared)));

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
