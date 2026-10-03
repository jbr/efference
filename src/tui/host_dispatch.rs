//! The host's own routes, served to the UI thread (docs/tui.md).
//!
//! These are the routes addressed to the *host* rather than to the attached
//! session — the listing, the end, and the ones the new-session form needs — so
//! they run against a client re-based at the socket root, where they live under
//! either binding (docs/control-api.md).
//!
//! It is a separate task from the UI loop because the loop is blocking and these
//! are async, and a separate *module* because the form needs them from containers
//! that are not the full-screen loop: the inline viewport has no `ui_loop` and
//! still has to preview and create (backlog/new-session-form.md).

use super::{HostCmd, TuiEvent};
use crate::client::{CatalogStatus, Client, ConnectionCatalog};

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
                // The form opens on what the host has stored; every copy that is not
                // fresh is read (and awaited) next, each in its own task so a slow
                // provider holds up nothing else.
                let stale = context
                    .as_ref()
                    .map(|context| unfresh_connections(&context.catalog))
                    .unwrap_or_default();
                if events.send(TuiEvent::NewSession(context)).is_err() {
                    break;
                }
                if let Ok(cwd) = origin.as_deref() {
                    spawn_catalog_reads(&client, cwd, stale, None, &events);
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
            HostCmd::Archived => {
                let rows = match origin.as_deref() {
                    Ok(cwd) => client
                        .archived_sessions(cwd)
                        .await
                        .map(|rows| {
                            let rows = rows.into_iter().map(|archived| archived.row).collect();
                            crate::tui::resume_list::ResumeRow::from_inventory(
                                rows,
                                crate::tui::resume_list::now_ms(),
                            )
                        })
                        .map_err(|e| format!("listing archived sessions: {e}")),
                    Err(e) => Err(format!("reading the working directory: {e}")),
                };
                if events.send(TuiEvent::Archived(rows)).is_err() {
                    break;
                }
                continue;
            }
            // Reviving takes seconds (environment, MCP mounts); the page shows
            // `resuming…` meanwhile, and the answer is the id to switch to.
            HostCmd::Resume(id) => {
                let resumed = match origin.as_deref() {
                    Ok(cwd) => client
                        .patch_session(
                            &id,
                            &crate::daemon::PatchSession::activate(cwd.to_path_buf()),
                        )
                        .await
                        .map(|_| id.clone())
                        .map_err(|e| format!("resuming {id}: {e}")),
                    Err(e) => Err(format!("reading the working directory: {e}")),
                };
                if events.send(TuiEvent::Resumed(resumed)).is_err() {
                    break;
                }
                continue;
            }
            HostCmd::Catalog {
                connection,
                max_age,
            } => {
                if let Ok(cwd) = origin.as_deref() {
                    spawn_catalog_reads(&client, cwd, vec![connection], max_age, &events);
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

/// The connections whose catalog copy is not fresh: the ones the form reads
/// again as it opens. A connection with no credential is left out, since the host
/// attempts no read for it (docs/model-catalog.md).
pub(in crate::tui) fn unfresh_connections(catalog: &[ConnectionCatalog]) -> Vec<String> {
    catalog
        .iter()
        .filter(|entry| {
            !matches!(
                entry.status,
                CatalogStatus::Fresh | CatalogStatus::Unavailable
            )
        })
        .map(|entry| entry.connection.clone())
        .collect()
}

/// Read each named connection's catalog entry, awaiting the provider when the
/// stored copy is older than `max_age` seconds, and deliver each answer as a
/// [`TuiEvent::Catalog`]. One task per connection: the host bounds a read at 30
/// seconds, and a slow connection should not hold up the others.
pub(in crate::tui) fn spawn_catalog_reads(
    client: &Client,
    cwd: &std::path::Path,
    connections: Vec<String>,
    max_age: Option<u64>,
    events: &crossbeam_channel::Sender<TuiEvent>,
) {
    for connection in connections {
        let (client, cwd, events) = (client.clone(), cwd.to_path_buf(), events.clone());
        tokio::spawn(async move {
            let result = client
                .catalog_connection(&connection, &cwd, None, max_age)
                .await
                .map_err(|e| format!("reading {connection}'s models: {e}"));
            let _ = events.send(TuiEvent::Catalog { connection, result });
        });
    }
}

/// The new-session form's opening read (docs/tui.md): where a session would be
/// created, the connections the host has configured for it, and their stored
/// model catalogs. Both come from
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
    // The stored copies, never waiting on a provider. A failed read is an empty
    // catalog, not a form that never opens: the picker still offers every
    // connection and the typed-id path.
    let catalog = client.catalog(&cwd, None).await.unwrap_or_default();
    Ok(crate::tui::session_form::NewSessionContext {
        cwd,
        connections,
        catalog,
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
        env: Some(env),
    };
    client
        .create_session(&request)
        .await
        .map_err(|e| format!("creating the session: {e}"))
}
