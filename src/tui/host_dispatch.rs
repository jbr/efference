//! The host's own routes, served to the UI thread (docs/tui.md).
//!
//! These are the routes addressed to the *host* rather than to the attached
//! session — the listing, the end, and the three the new-session form needs — so
//! they run against a client re-based at the socket root, where they live under
//! either binding (docs/control-api.md).
//!
//! It is a separate task from the UI loop because the loop is blocking and these
//! are async, and a separate *module* because the form needs them from containers
//! that are not the full-screen loop: the inline viewport has no `ui_loop` and
//! still has to preview and create (backlog/new-session-form.md).

use super::{HostCmd, TuiEvent};
use crate::client::Client;
use crate::tui::session_form::MenuAnswer;

/// Answer host commands until the UI thread goes away.
///
/// The creating client's own working root and environment are read here, on the
/// tokio side, and never sent to the UI thread (docs/env.md): the form displays
/// the root and nothing displays the environment. Both are read once, so a
/// session created later in the run gets the same base as one created at
/// startup.
pub(super) async fn serve(
    client: Client,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<HostCmd>,
    events: crossbeam_channel::Sender<TuiEvent>,
) {
    let origin = std::env::current_dir();
    while let Some(cmd) = commands.recv().await {
        match cmd {
            HostCmd::NewSession => {
                let context = match origin.as_deref() {
                    Ok(cwd) => new_session_context(&client, cwd).await,
                    Err(e) => Err(format!("reading the working directory: {e}")),
                };
                if events.send(TuiEvent::NewSession(context)).is_err() {
                    break;
                }
                continue;
            }
            HostCmd::Preview(options) => {
                let resolved = match origin.as_deref() {
                    Ok(cwd) => {
                        let mut request =
                            crate::daemon::SessionOptionsRequest::new(cwd.to_path_buf());
                        request.options = options;
                        client
                            .session_options(&request)
                            .await
                            .map_err(|e| format!("resolving the draft: {e}"))
                    }
                    Err(e) => Err(format!("reading the working directory: {e}")),
                };
                if events.send(TuiEvent::Preview(resolved)).is_err() {
                    break;
                }
                continue;
            }
            HostCmd::Create(options) => {
                let created = create_session(&client, origin.as_deref(), options).await;
                if events.send(TuiEvent::Created(created)).is_err() {
                    break;
                }
                continue;
            }
            HostCmd::Recents(request) => {
                let recents = match origin.as_deref() {
                    Ok(cwd) => {
                        // The form holds the inventory the menu index came
                        // from — but the dispatcher holds the client, so the
                        // name resolves here, against a fresh inventory read.
                        // A connection renamed mid-menu answers empty, which
                        // is a typed-ID-only menu rather than an error.
                        let empty = || MenuAnswer {
                            role: request.role,
                            connection: request.connection,
                            models: Vec::new(),
                        };
                        match client.inference_inventory().await {
                            Ok(inventory) => match inventory.get(request.connection) {
                                // The catalog is a suggestion source: a failed
                                // read leaves an empty menu with the typed-ID
                                // row, it does not fail the menu.
                                Some(row) => Ok(client
                                    .catalog_connection(&row.name, cwd, None, None)
                                    .await
                                    .map(|catalog| MenuAnswer {
                                        models: menu_models(catalog),
                                        ..empty()
                                    })
                                    .unwrap_or_else(|_| empty())),
                                None => Ok(empty()),
                            },
                            Err(e) => Err(format!("reading the host's connections: {e}")),
                        }
                    }
                    Err(e) => Err(format!("reading the working directory: {e}")),
                };
                if events.send(TuiEvent::Recents(recents)).is_err() {
                    break;
                }
                continue;
            }
            // An end is followed by a re-read, and the host answers a `DELETE`
            // only once teardown has run, so the listing that comes back is the
            // finished state.
            HostCmd::Archive(id) => {
                if let Err(e) = client.archive_session(&id).await {
                    let _ = events.send(TuiEvent::Sessions(Err(format!("ending {id}: {e}"))));
                    continue;
                }
            }
            HostCmd::List => {}
        }
        let listing = client
            .sessions()
            .await
            .map_err(|e| format!("listing the host's sessions: {e}"));
        if events.send(TuiEvent::Sessions(listing)).is_err() {
            break; // UI thread gone.
        }
    }
}

/// A connection's catalog as a flat menu: past picks first in rank order, then
/// the rest in the catalog's order, each labelled with its name where it has one.
fn menu_models(catalog: crate::client::ConnectionCatalog) -> Vec<crate::client::ModelSuggestion> {
    let mut rows = catalog.models;
    rows.sort_by_key(|row| row.recent.unwrap_or(u32::MAX));
    rows.into_iter()
        .map(|row| crate::client::ModelSuggestion {
            label: row.row.name.unwrap_or_else(|| row.row.model.clone()),
            model: row.row.model,
        })
        .collect()
}

/// The new-session form's opening read (docs/tui.md): where a session would be
/// created, and the connections the host has configured for it. Both come from
/// the client that is creating — the working root is this process's, as the
/// environment creation carries will be — so the form shows what will actually
/// be sent rather than a root it asked for and cannot supply an environment at.
pub(in crate::tui) async fn new_session_context(
    client: &Client,
    cwd: &std::path::Path,
) -> Result<crate::tui::session_form::NewSessionContext, String> {
    let cwd = cwd.to_path_buf();
    let connections = client
        .inference_inventory()
        .await
        .map_err(|e| format!("reading the host's connections: {e}"))?;
    // Best-effort like every history read: an unreadable store is an empty
    // list, not a form that never opens.
    let recent_picks = client.recent_picks(&cwd, None).await.unwrap_or_default();
    Ok(crate::tui::session_form::NewSessionContext {
        cwd,
        connections,
        recent_picks,
    })
}

/// `POST /sessions` from the form's draft. The environment is captured here and
/// moved straight into the request: it is never sent to the UI thread, never
/// rendered, and the error quotes the host's answer rather than the request
/// (docs/env.md).
async fn create_session(
    client: &Client,
    origin: Result<&std::path::Path, &std::io::Error>,
    options: crate::inference::SessionOptions,
) -> Result<String, String> {
    let cwd = origin
        .map_err(|e| format!("reading the working directory: {e}"))?
        .to_path_buf();
    let env = crate::env_table::EnvSnapshot::capture()
        .map_err(|e| format!("capturing the environment: {e}"))?
        .into_entries();
    let request = crate::daemon::CreateSession {
        name: None,
        cwd,
        config: None,
        options,
        upstream: Vec::new(),
        autonomous: false,
        reauth: Vec::new(),
        keep_scratch: false,
        // A session created by a person at a keyboard has no creator to end with
        // (docs/tools.md).
        detached: false,
        env,
    };
    client
        .create_session(&request)
        .await
        .map_err(|e| format!("creating the session: {e}"))
}
