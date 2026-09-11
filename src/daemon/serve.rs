//! Booting one session's daemon: start its runtime, bind the listeners, and
//! wait for the loop (docs/control-channel.md).

use super::*;

/// Run the daemon: start the session under the listener's `Swansong`, bind the
/// control socket (and the optional TCP listener), and wait for the loop. The
/// loop blocks until a `POST /turns` arrives, so an idle daemon simply waits —
/// retiring argv-seeding and the "backgrounded run can't get its opening
/// prompt" problem (docs/README.md).
pub async fn serve<S: Spotter + Send + 'static>(
    boot: SessionBoot<S>,
    socket: PathBuf,
    bind: Option<tcp::TcpBind>,
) -> Result<(), Error> {
    let cwd = boot.paths.cwd().to_path_buf();
    // Trillium owns the signal registry: it registers SIGINT/SIGTERM against the
    // `Swansong` the server is configured with and drives graceful shutdown through
    // it. The session runtime is a child of that same `Swansong`, so a C-c that
    // stops the listener also cancels the loop — which otherwise parks forever in
    // `read_user_turn` (the `kill -9` hang) — and the runtime's teardown guard holds
    // the server's shutdown open until background children are reaped.
    let swansong = trillium_tokio::Swansong::new();
    let runtime = SessionRuntime::start(boot, &swansong);
    let shared = Arc::clone(runtime.session());

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
        .with_swansong(swansong.clone())
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(handler(
            Transport::Socket {
                auth: web_auth.clone(),
            },
            App::session(&cwd),
        ));
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
            handler(
                Transport::Web {
                    auth: Arc::clone(&auth),
                },
                App::session(&cwd),
            ),
            swansong.clone(),
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

    // `server` is held — not dropped — for the whole await so the listener stays
    // up; it drops on return, after the loop has ended. A loop that ends on its own
    // (end of input) leaves the listener to stop with the process.
    let outcome = runtime.finished().await;
    drop(server);
    outcome
}
