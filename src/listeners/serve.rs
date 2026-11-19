//! Serving a host (docs/control-channel.md, docs/web-bind.md): the control socket, the
//! optional TCP listener, and the [`handler()`] each one mounts.

use super::*;
use crate::host::Host;
use crate::host::permissions;
use crate::inference::api::Error;
use crate::listeners::handler::App;
use crate::listeners::handler::Transport;
use crate::listeners::handler::handler;
use crate::listeners::tcp::TcpBind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trillium_server_common::ServerHandle;

/// Run a host: bind the control socket and the optional TCP listener over
/// `host`, and serve until shutdown. Sessions arrive by `POST /sessions`.
///
/// A bind that fails shuts the host down before the error returns. The control socket
/// is already serving when the TCP bind runs, and the caller's tasks under the host's
/// `Swansong` (the boot resume, the terminal title) would otherwise keep the process up
/// as a host without its TCP listener, the error unreported until a shutdown that might
/// never come.
pub async fn serve_host(
    host: Arc<Host>,
    socket: PathBuf,
    bind: Option<TcpBind>,
) -> Result<(), Error> {
    serve_host_with_state(host, socket, bind, crate::platform::state::user_scoped()).await
}

/// [`serve_host`] with the user state database the TCP bind keeps its devices and
/// sign-ins in injected, as [`tcp::arm`] takes it: the interface a test uses
/// to keep them out of the real user-scoped file.
pub(crate) async fn serve_host_with_state(
    host: Arc<Host>,
    socket: PathBuf,
    bind: Option<TcpBind>,
    state: PathBuf,
) -> Result<(), Error> {
    let served = match bind_listeners(&host, &socket, bind, state).await {
        Ok((control, _web)) => {
            // The listener's shutdown completes only when every session's teardown
            // guard has dropped: the runtimes are children of the same `Swansong`.
            control.await;
            Ok(())
        }
        Err(e) => {
            host.swansong().shut_down().await;
            Err(e)
        }
    };
    // Every session has released its binding by now; this reaps what is still starting
    // or running (docs/tools.md).
    crate::tools::lsp::shut_down(Arc::clone(host.language_servers())).await;
    served
}

/// Bind and spawn the control socket's server, then the TCP listener's when there is a
/// bind, returning their handles.
async fn bind_listeners(
    host: &Arc<Host>,
    socket: &Path,
    bind: Option<TcpBind>,
    state: PathBuf,
) -> Result<(ServerHandle, Option<ServerHandle>), Error> {
    // The state beside this socket and the socket itself are the host's whole
    // on-disk footprint; tighten them before anything is served from here
    // (docs/state-store.md). The directory is created when missing so the
    // first boot is tightened too, not only later ones.
    if let Some(dir) = socket.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("could not prepare {}: {e}", dir.display());
        }
        permissions::tighten_state_dir(dir);
    }
    let web = bind.map(|bind| tcp::arm(bind, state));
    let web_auth = web.as_ref().map(|(auth, _)| Arc::clone(auth));
    let server = trillium_tokio::config()
        .with_swansong(host.swansong().clone())
        .with_shared_state(Arc::clone(host))
        .listeners()
        .bind_uds(socket)
        .map_err(|e| -> Error { format!("failed to bind {}: {e}", socket.display()).into() })?
        .spawn(handler(
            Transport::Socket {
                auth: web_auth.clone(),
            },
            App::Host,
        ));
    // The bind minted the socket under the process umask; owner-only is the
    // contract (docs/control-channel.md) — a connect needs write permission,
    // so this is exactly the accounts that may reach the channel.
    permissions::tighten_socket(socket);
    log::info!("host socket: {}", socket.display());
    let mut web_server = None;
    if let Some((auth, listener)) = web {
        let open = listener.open;
        let addr = listener.addr.clone();
        let handle = tcp::spawn(
            &listener,
            handler(
                Transport::Web {
                    auth: Arc::clone(&auth),
                },
                App::Host,
            ),
            host.swansong().clone(),
            Arc::clone(host),
        )
        .map_err(|e| -> Error { format!("failed to bind {addr}: {e}").into() })?;
        // Bound before the link is minted: a bind to port 0 learns its port here.
        if let Some(bound) = handle.info().await.tcp_socket_addr() {
            auth.bound_port(bound.port());
        }
        log::info!("web: {} (bound {addr})", auth.public_base());
        match auth.mint_link() {
            Ok(link) => {
                log::info!("  {link}");
                if open && webbrowser::open(&link).is_err() {
                    log::warn!("could not open a browser for {link}");
                }
            }
            Err(e) => log::warn!("could not mint a sign-in link: {e}"),
        }
        web_server = Some(handle);
    }
    Ok((server, web_server))
}

#[cfg(test)]
mod test;
