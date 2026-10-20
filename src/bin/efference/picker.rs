//! Bare `efference` with no `--name`: an inline picker before the full-screen
//! view (docs/operating.md).
//!
//! The top page is the entry that creates a session here, then the host's
//! sessions whose cwd is this directory — the listing `GET /sessions` returns,
//! scoped the way every project-local client scopes it
//! (docs/control-channel.md) — then, when this directory has dormant sessions, an
//! entry that opens the resume page: every resumable archived session here, most
//! recent first, under a query line. A live row is the attach `--name` does,
//! the create entry opens the new-session form, and an archived row is revived
//! with `PATCH {"state": "active"}` (docs/control-api.md) and then attached the
//! same way. The archived rows come
//! from `GET /sessions?state=archived` when a host is running and from the
//! host-free scan in `src/inventory.rs` when none is.
//!
//! The pages are ratatui widgets in an inline viewport, the same list and
//! theme the new-session form draws (`app.rs`, docs/tui.md).

use super::*;
use efference::client::ListedSession;
use efference::inventory::InventoryRow;
use efference::tui::inline;
use efference::tui::resume_list::{ResumeRow, now_ms};

mod app;

/// One session the picker can attach to, with the host that serves it — a host
/// per row because a project host and the user's host can both be live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) host: PathBuf,
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    /// What the session is doing, in the word the listing carries.
    pub(crate) status: String,
}

/// What the live hosts hold, split by whether it belongs to this directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Menu {
    /// Sessions whose cwd is this directory, in the order the hosts listed them.
    pub(crate) rows: Vec<Row>,
    /// How many sessions the same hosts hold in other directories. Counted and
    /// named, not listed: attaching from here to a session rooted elsewhere
    /// would put the TUI's working directory and the session's out of step.
    pub(crate) elsewhere: usize,
    /// The archived sessions here that can be resumed, most recently active
    /// first.
    pub(crate) archived: Vec<ResumeRow>,
}

/// What the person chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Attach to this session on this host.
    Attach { host: PathBuf, id: String },
    /// Revive this archived session in this directory, then attach.
    Resume { id: String },
    /// Create a session in this directory.
    Create,
}

/// Split the live hosts' listings into this directory's sessions and a count of
/// the rest.
pub(crate) fn menu(listings: &[(PathBuf, Vec<ListedSession>)], here: &Path) -> Menu {
    let mut menu = Menu::default();
    for (host, sessions) in listings {
        for session in sessions {
            let mine = session
                .cwd
                .as_ref()
                .is_none_or(|cwd| same_dir(Path::new(cwd), here));
            if mine {
                menu.rows.push(Row {
                    host: host.clone(),
                    id: session.id.clone(),
                    title: session.title.clone(),
                    status: session.status.clone(),
                });
            } else {
                menu.elsewhere += 1;
            }
        }
    }
    menu
}

impl Menu {
    /// Add the archived sessions from inventory rows (most recently active
    /// first, as the scan and the route return them). A row with a `problem` is
    /// not offered: it could not be resumed.
    pub(crate) fn with_archived(mut self, rows: Vec<InventoryRow>, now_ms: u64) -> Self {
        self.archived = ResumeRow::from_inventory(rows, now_ms);
        self
    }
}

/// A path with the home directory written as `~`, for a prompt line.
pub(crate) fn tilde(path: &Path) -> String {
    under_home(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

/// [`tilde`] against a given home, so the rewrite is testable without touching
/// the process environment.
pub(crate) fn under_home(path: &Path, home: Option<&Path>) -> String {
    let display = path.display().to_string();
    let Some(home) = home else {
        return display;
    };
    let home = home.display().to_string();
    if home.is_empty() {
        return display;
    }
    match display.strip_prefix(&home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => display,
    }
}

/// Every live host's session listing, in candidate order.
pub(crate) async fn live_listings() -> Result<Vec<(PathBuf, Vec<ListedSession>)>, CliError> {
    let mut listings = Vec::new();
    for host in host_candidates() {
        if !is_live(&host) {
            continue;
        }
        let sessions = ControlClient::new(&host)
            .sessions()
            .await
            .map_err(|error| format!("listing {}: {error}", host.display()))?;
        listings.push((host, sessions));
    }
    Ok(listings)
}

/// The verb: gather what the live hosts hold here, ask, and hand off to the
/// path the explicit flag would have taken.
pub(crate) async fn run(display: efference::inference::TuiConfig, bind: Option<&str>) -> CliResult {
    let theme = display.theme.clone();
    let here = std::env::current_dir()?;
    let listings = live_listings().await?;
    if listings.is_empty() {
        // Nothing live to pick from: the archived sessions here, read from disk
        // since there is no host to ask, are all the menu can hold.
        let archived = scan_archived(&here).await;
        let creation = match archived.is_empty() || !inline::interactive() {
            true => island::Creation::Form {
                theme: theme.clone(),
            },
            false => {
                let menu = Menu::default().with_archived(archived, now_ms());
                match app::ask(&menu, &here, inline::styles(theme.clone())?)? {
                    None => return Ok(()),
                    Some(Pick::Resume { id }) => island::Creation::Resume { id },
                    Some(_) => island::Creation::Form {
                        theme: theme.clone(),
                    },
                }
            }
        };
        // Nowhere to create or resume: start a host owned by this terminal and
        // do it there (docs/operating.md). A `bind` makes it the browser island
        // rather than the TUI.
        return island::run(bind, creation, display).await;
    }
    // The first live candidate: the host the form creates on, and so the one
    // an archived session is revived on (docs/control-channel.md).
    let archived = match ControlClient::new(&listings[0].0)
        .archived_sessions(&here)
        .await
    {
        Ok(rows) => rows.into_iter().map(|archived| archived.row).collect(),
        Err(error) => {
            eprintln!("could not list archived sessions: {error}");
            Vec::new()
        }
    };
    let menu = menu(&listings, &here).with_archived(archived, now_ms());
    // Without a terminal to draw in there is nothing to ask with; resolve the
    // way an explicit target resolves instead.
    if !inline::interactive() {
        return tui(resolve_client_target(None).await?, display).await;
    }
    let Some(pick) = app::ask(&menu, &here, inline::styles(theme.clone())?)? else {
        return Ok(());
    };
    let host = listings[0].0.clone();
    match hosted_target(pick, &host).await? {
        Some(target) => tui(target, display).await,
        None => create_through_form(host, display).await,
    }
}

/// Where a pick on a running `host` attaches: a live row as it is, an archived
/// one after the host has revived it. `None` for the create entry, which has
/// the form still to run.
async fn hosted_target(pick: Pick, host: &Path) -> Result<Option<ClientTarget>, CliError> {
    match pick {
        Pick::Attach { host, id } => Ok(Some(ClientTarget::hosted(host, &id))),
        Pick::Resume { id } => {
            resume_on(&ControlClient::new(host), &id).await?;
            Ok(Some(ClientTarget::hosted(host.to_path_buf(), &id)))
        }
        Pick::Create => Ok(None),
    }
}

/// The inventory rows under `here`, read from disk: what the picker lists when
/// no host is there to ask.
async fn scan_archived(here: &Path) -> Vec<InventoryRow> {
    let here = here.to_path_buf();
    tokio::task::spawn_blocking(move || efference::inventory::Inventory::new().refresh(&here))
        .await
        .unwrap_or_default()
}

/// Revive the archived session `id` of this directory on `client`'s host. The
/// host re-establishes the environment and mounts the session's MCP servers,
/// which takes seconds, so a line says so first and is erased when it is done.
pub(crate) async fn resume_on(client: &ControlClient, id: &str) -> Result<(), CliError> {
    use std::io::IsTerminal;
    let status = std::io::stderr().is_terminal();
    if status {
        eprint!("resuming {id}…");
    }
    let request = efference::daemon::PatchSession::activate(std::env::current_dir()?);
    let patched = client.patch_session(id, &request).await;
    if status {
        eprint!("\r\x1b[2K");
    }
    patched
        .map(drop)
        .map_err(|error| format!("resuming {id}: {error}").into())
}

/// Create a session on `host` through the new-session form and attach to it.
/// The picker answered "what are you doing"; the form is the rest of that
/// question, so it runs inline where the picker just was rather than creating
/// from bare defaults (docs/tui.md). `efference control create` is the
/// non-interactive path.
pub(crate) async fn create_through_form(
    host: PathBuf,
    display: efference::inference::TuiConfig,
) -> CliResult {
    let client = ControlClient::new(&host);
    let Some(id) = efference::tui::session_form::run_hosted(client, display.theme.clone()).await?
    else {
        return Ok(());
    };
    tui(ClientTarget::hosted(host, &id), display).await
}

#[cfg(test)]
mod test;
