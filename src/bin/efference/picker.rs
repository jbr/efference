//! `efference tui` with no flags: an inline picker before the full-screen view
//! (docs/operating.md).
//!
//! The rows are the host's sessions whose cwd is this directory — the listing
//! `GET /sessions` returns, scoped the way every project-local client scopes it
//! (docs/control-channel.md) — then this directory's archived sessions, then one
//! entry that creates a session here. The outcomes converge on the paths the
//! explicit flags take: a live row is the attach `tui` does, the create entry is
//! `tui --new`, and an archived row is revived with
//! `PATCH {"state": "active"}` (docs/control-api.md) and then attached the same
//! way. The archived section is the [`ARCHIVED_SHOWN`] most recently active
//! sessions whose logs can be resumed, from `GET /sessions?state=archived` when a
//! host is running and from the host-free scan in `src/inventory.rs` when none is.
//!
//! The prompts are `efference setup`'s ([`Prompter`]), so the flow is exercised
//! against a scripted prompter rather than a terminal (`picker/test.rs`).

use super::*;
use efference::client::SessionSummary;
use efference::inventory::InventoryRow;
use efference::setup::{Cancelled, Choice, Prompter, Tone, terminal::Terminal};

/// One session the picker can attach to, with the host that serves it — a host
/// per row because a project host and the user's host can both be live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) host: PathBuf,
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) created_by: Option<String>,
}

/// How many archived sessions the picker lists. Past that the count says how
/// many more there are; narrowing them is a search, not a longer list.
pub(crate) const ARCHIVED_SHOWN: usize = 10;

/// One dormant session in this directory the picker offers to resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Archived {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    /// How long ago it was last active, as the row shows it.
    pub(crate) age: Option<String>,
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
    /// The most recently active archived sessions here that can be resumed, at
    /// most [`ARCHIVED_SHOWN`].
    pub(crate) archived: Vec<Archived>,
    /// How many resumable archived sessions are not in `archived`.
    pub(crate) archived_more: usize,
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
pub(crate) fn menu(listings: &[(PathBuf, Vec<SessionSummary>)], here: &Path) -> Menu {
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
                    created_by: session.created_by.clone(),
                });
            } else {
                menu.elsewhere += 1;
            }
        }
    }
    menu
}

impl Menu {
    /// Add the archived section from inventory rows (most recently active
    /// first, as the scan and the route return them). A row with a `problem` is
    /// not offered: it could not be resumed.
    pub(crate) fn with_archived(mut self, rows: Vec<InventoryRow>, now_ms: u64) -> Self {
        let mut resumable = rows.into_iter().filter(|row| row.problem.is_none());
        self.archived = resumable
            .by_ref()
            .take(ARCHIVED_SHOWN)
            .map(|row| Archived {
                age: row.last_active_at.map(|at| age(now_ms, at)),
                id: row.id,
                title: row.title,
            })
            .collect();
        self.archived_more = resumable.count();
        self
    }
}

/// `then_ms` as time before `now_ms`, to the largest whole unit: "3h ago".
pub(crate) fn age(now_ms: u64, then_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(then_ms) / 1000;
    match seconds {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The second clause of a row: the title the triage model keeps current
/// (docs/session-title.md), and the session that commissioned this one when one
/// did (docs/tools.md).
fn detail(row: &Row) -> String {
    let title = row.title.clone().unwrap_or_else(|| "untitled".to_string());
    match &row.created_by {
        Some(by) => format!("{title} · created by {by}"),
        None => title,
    }
}

/// The menu as prompt entries: a row per live session here, a row per archived
/// one (its title, or its id when it has none, with its age), then the create
/// entry, which is last so it is always in the same place.
pub(crate) fn choices(menu: &Menu, here: &Path) -> Vec<Choice> {
    let mut choices: Vec<Choice> = menu
        .rows
        .iter()
        .map(|row| Choice::new(row.id.clone()).detail(detail(row)))
        .collect();
    choices.extend(menu.archived.iter().map(|session| {
        let label = session.title.as_deref().unwrap_or(&session.id);
        let detail = match &session.age {
            Some(age) => format!("archived · {age}"),
            None => "archived".to_string(),
        };
        Choice::new(label).detail(detail)
    }));
    choices.push(Choice::new("create a new session").detail(format!("in {}", tilde(here))));
    choices
}

/// Ask. The rows come first and the cursor starts on the first one, so a single
/// session here is one Enter away.
pub(crate) fn pick<P: Prompter>(
    prompter: &mut P,
    menu: &Menu,
    here: &Path,
) -> Result<Pick, Cancelled> {
    let here_label = tilde(here);
    if menu.elsewhere > 0 {
        prompter.say(
            Tone::Plain,
            &format!(
                "{} session{} on this host {} in other directories; run `efference` there to see {}.",
                menu.elsewhere,
                if menu.elsewhere == 1 { "" } else { "s" },
                if menu.elsewhere == 1 { "is" } else { "are" },
                if menu.elsewhere == 1 { "it" } else { "them" }
            ),
        );
    }
    // With nothing here to attach to or resume, the menu's one entry is
    // "create", and asking for it is a keystroke that says nothing: the form
    // comes next.
    if menu.rows.is_empty() && menu.archived.is_empty() {
        return Ok(Pick::Create);
    }
    if menu.archived_more > 0 {
        prompter.say(
            Tone::Plain,
            &format!(
                "{} more archived session{} here {} not listed.",
                menu.archived_more,
                if menu.archived_more == 1 { "" } else { "s" },
                if menu.archived_more == 1 { "is" } else { "are" },
            ),
        );
    }
    let choices = choices(menu, here);
    let index = prompter.select(&format!("sessions in {here_label}"), &choices)?;
    Ok(if let Some(row) = menu.rows.get(index) {
        Pick::Attach {
            host: row.host.clone(),
            id: row.id.clone(),
        }
    } else if let Some(session) = menu.archived.get(index - menu.rows.len()) {
        Pick::Resume {
            id: session.id.clone(),
        }
    } else {
        Pick::Create
    })
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
pub(crate) async fn live_listings() -> Result<Vec<(PathBuf, Vec<SessionSummary>)>, CliError> {
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
pub(crate) async fn run(
    theme: Option<String>,
    images: efference::tui::ImageMode,
    frame_cost: bool,
    bind: Option<&str>,
) -> CliResult {
    let here = std::env::current_dir()?;
    let listings = live_listings().await?;
    if listings.is_empty() {
        // A session on its own socket needs no host, so the discovery path
        // still gets its chance before the message about binding one.
        if let Ok(target) = resolve_client_target(None, None).await {
            return tui(target, theme, images, frame_cost).await;
        }
        // Nothing live to pick from: the archived sessions here, read from disk
        // since there is no host to ask, are all the menu can hold.
        let archived = scan_archived(&here).await;
        let mut terminal = Terminal::open().ok().filter(|_| !archived.is_empty());
        let creation = match terminal.as_mut() {
            None => island::Creation::Form {
                theme: theme.clone(),
            },
            Some(terminal) => {
                let menu = Menu::default().with_archived(archived, now_ms());
                match pick(terminal, &menu, &here) {
                    Err(Cancelled) => return Ok(()),
                    Ok(Pick::Resume { id }) => island::Creation::Resume { id },
                    Ok(_) => island::Creation::Form {
                        theme: theme.clone(),
                    },
                }
            }
        };
        if let Some(terminal) = terminal.as_mut() {
            terminal.erase();
        }
        // Nowhere to create or resume: start a host owned by this terminal and
        // do it there (docs/operating.md). A `bind` makes it the browser island
        // rather than the TUI.
        return island::run(bind, creation, theme, images, frame_cost).await;
    }
    // The first live candidate: the host `tui --new` creates on, and so the one
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
    let Ok(mut terminal) = Terminal::open() else {
        return tui(
            resolve_client_target(None, None).await?,
            theme,
            images,
            frame_cost,
        )
        .await;
    };
    let pick = match pick(&mut terminal, &menu, &here) {
        Ok(pick) => pick,
        Err(Cancelled) => return Ok(()),
    };
    // Answered: nothing of the question is left behind once the form or the TUI
    // takes over, as with a terminal island (docs/operating.md).
    terminal.erase();
    let host = listings[0].0.clone();
    match hosted_target(pick, &host).await? {
        Some(target) => tui(target, theme, images, frame_cost).await,
        None => create_through_form(host, theme, images, frame_cost).await,
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
/// from bare defaults (docs/tui.md). `tui --new` is the non-interactive path.
pub(crate) async fn create_through_form(
    host: PathBuf,
    theme: Option<String>,
    images: efference::tui::ImageMode,
    frame_cost: bool,
) -> CliResult {
    let client = ControlClient::new(&host);
    let Some(id) = efference::tui::session_form::run_hosted(client, theme.clone()).await? else {
        return Ok(());
    };
    tui(ClientTarget::hosted(host, &id), theme, images, frame_cost).await
}

#[cfg(test)]
mod test;
