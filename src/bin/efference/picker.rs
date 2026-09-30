//! `efference tui` with no flags: an inline picker before the full-screen view
//! (docs/operating.md).
//!
//! The rows are the host's sessions whose cwd is this directory — the listing
//! `GET /sessions` returns, scoped the way every project-local client scopes it
//! (docs/control-channel.md) — plus one entry that creates a session here. Both
//! outcomes converge on the paths the explicit flags take: a row is the attach
//! `tui` does, and the create entry is `tui --new`.
//!
//! The prompts are `efference setup`'s ([`Prompter`]), so the flow is exercised
//! against a scripted prompter rather than a terminal (`picker/test.rs`).

use super::*;
use efference::client::SessionSummary;
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

/// What the live hosts hold, split by whether it belongs to this directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Menu {
    /// Sessions whose cwd is this directory, in the order the hosts listed them.
    pub(crate) rows: Vec<Row>,
    /// How many sessions the same hosts hold in other directories. Counted and
    /// named, not listed: attaching from here to a session rooted elsewhere
    /// would put the TUI's working directory and the session's out of step.
    pub(crate) elsewhere: usize,
}

/// What the person chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Attach to this session on this host.
    Attach { host: PathBuf, id: String },
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

/// The menu as prompt entries: a row per session here, then the create entry,
/// which is last so it is always in the same place.
pub(crate) fn choices(menu: &Menu, here: &Path) -> Vec<Choice> {
    let mut choices: Vec<Choice> = menu
        .rows
        .iter()
        .map(|row| Choice::new(row.id.clone()).detail(detail(row)))
        .collect();
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
    // With nothing here to attach to, the menu's one entry is "create", and
    // asking for it is a keystroke that says nothing: the form comes next.
    if menu.rows.is_empty() {
        return Ok(Pick::Create);
    }
    let choices = choices(menu, here);
    let index = prompter.select(&format!("sessions in {here_label}"), &choices)?;
    Ok(match menu.rows.get(index) {
        Some(row) => Pick::Attach {
            host: row.host.clone(),
            id: row.id.clone(),
        },
        None => Pick::Create,
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
        // Nothing to pick from and nowhere to create: start a host owned by
        // this terminal and create here (docs/operating.md). A `bind` makes it
        // the browser island rather than the TUI.
        let creation = island::Creation::Form {
            theme: theme.clone(),
        };
        return island::run(bind, creation, theme, images, frame_cost).await;
    }
    let menu = menu(&listings, &here);
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
    match pick {
        Pick::Attach { host, id } => {
            tui(ClientTarget::hosted(host, &id), theme, images, frame_cost).await
        }
        Pick::Create => {
            // The first live candidate, which is the host `tui --new` creates
            // on (docs/control-channel.md).
            let host = listings[0].0.clone();
            create_through_form(host, theme, images, frame_cost).await
        }
    }
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
