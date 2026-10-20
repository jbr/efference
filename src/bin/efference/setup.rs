//! `efference setup` — the interactive first-run path (docs/operating.md), and
//! what a bare `efference` does: setup when there is no configuration; the offer
//! to set this directory up when there is but the directory has no project
//! config and no sessions; otherwise the picker.

use super::*;
use efference::setup::{Existing, LiveProbe, Prompter as _, Tone, terminal::Terminal};

/// The user configuration path under this process's `HOME`.
fn default_config_path() -> Result<PathBuf, CliError> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set; pass --config <path>")?;
    Ok(PathBuf::from(home).join(efference::inference::CONFIG_RELATIVE_PATH))
}

/// Bare `efference`: setup on a machine with no configuration. Configured, a
/// directory with neither a project config nor sessions is offered setup; any
/// other opens the picker — pick a session here, or with no host running,
/// start one for this terminal.
///
/// `bind` is `--bind`, carried to whichever island this ends up starting so the
/// flag survives the questions in between and reaches the browser rather than
/// being dropped at the first branch (docs/operating.md).
pub(crate) async fn default_verb(bind: Option<&str>) -> CliResult {
    let path = default_config_path()?;
    if !path.exists() {
        return run(Some(path), bind).await;
    }
    let here = std::env::current_dir()?;
    let display = display()?;
    let configured = here.join(efference::config::DEFAULT_PATH).exists();
    if !configured {
        let listings = picker::live_listings().await?;
        if picker::menu(&listings, &here).rows.is_empty() {
            return offer_here(&mut Terminal::open()?, None, bind, display).await;
        }
    }
    picker::run(display, bind).await
}

/// Ask whether to set this directory up now, and on yes create a session here —
/// on the running host, or on an island — whose missing project config makes it
/// the setup profile (docs/operating.md).
///
/// On yes, everything `terminal` has printed is erased before the TUI takes the
/// screen, so quitting it returns to the shell as it was: the questions are
/// answered, and the session is where the conversation continues. `kept` is the
/// one line worth leaving, such as where the wizard wrote its file.
async fn offer_here(
    terminal: &mut Terminal,
    kept: Option<String>,
    bind: Option<&str>,
    display: efference::inference::TuiConfig,
) -> CliResult {
    let here = std::env::current_dir()?;
    // The history default is named in the question because it is what the
    // conversation is billed on; with no past pick the form names the current
    // model instead. Best-effort: an unreadable store reads as no history.
    let model = std::env::var_os("HOME").and_then(|home| {
        let store = efference::history::History::user(std::path::Path::new(&home));
        let scope = efference::history::Scope::for_request(&here, None);
        store
            .default(&scope, efference::history::now())
            .map(|candidate| format!("{}/{}", candidate.connection, candidate.model))
    });
    // Esc at the question is a decline like any other.
    let chosen =
        efference::setup::offer_project_setup(terminal, &picker::tilde(&here), model.as_deref())
            .unwrap_or_default();
    let Some(surface) = chosen else {
        return Ok(());
    };
    terminal.erase();
    if let Some(kept) = kept {
        terminal.say(Tone::Good, &kept);
    }
    if surface == efference::setup::Surface::Browser {
        // Always an island: a running host has its own web bind, or none, and
        // this choice never changes one (docs/operating.md). The address is
        // `--bind` when given, and a port the kernel picks otherwise.
        let creation = island::Creation::Form {
            theme: display.theme,
        };
        return island::run_browser(bind.unwrap_or(island::BIND), creation).await;
    }
    // The session's settings are chosen in the new-session form, as any other
    // session's are (docs/tui.md); in a directory with no project config the
    // session it creates is the setup profile.
    match cli::live_host() {
        Some(host) => picker::create_through_form(host, display).await,
        None => {
            let creation = island::Creation::Form {
                theme: display.theme.clone(),
            };
            island::run(bind, creation, display).await
        }
    }
}

/// The verb: ask, render, show, write.
///
/// `bind` reaches the hand-off's island, so `efference setup --bind 8080`
/// answers the setup offer in the browser on that address — the wizard asks
/// which surface, and the flag is the address, not a replacement for the
/// question (docs/operating.md).
pub(crate) async fn run(config: Option<PathBuf>, bind: Option<&str>) -> CliResult {
    let path = match config {
        Some(path) => path,
        None => default_config_path()?,
    };
    let existing_text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("could not read {}: {error}", path.display()).into()),
    };
    // A config this cannot read whole is refused rather than shown in part:
    // the connection it could not parse is the one most likely to need
    // repairing, and a shorter list looks complete (docs/operating.md).
    let existing = Existing::from_toml(&existing_text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut terminal = Terminal::open()?;
    let probe = LiveProbe::capture()?;
    terminal.say(
        Tone::Plain,
        &format!("efference setup · writing {}\n", path.display()),
    );
    let draft = match efference::setup::run(&mut terminal, &probe, &existing).await {
        Ok(draft) => draft,
        Err(cancelled) => {
            terminal.say(Tone::Note, &cancelled.to_string());
            std::process::exit(1);
        }
    };
    let text = efference::setup::render(&existing_text, &draft)?;
    terminal.say(Tone::Plain, "");
    terminal.say(Tone::Plain, &format!("{}:", path.display()));
    terminal.say(Tone::Plain, "");
    for line in text.lines() {
        terminal.say(Tone::Plain, &format!("    {line}"));
    }
    terminal.say(Tone::Plain, "");
    efference::setup::write(&path, &text)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    terminal.say(Tone::Good, &format!("Wrote {}.", path.display()));
    // A key recorded as an environment variable has to be in the environment
    // the host starts from, which for the hand-off is this one. One that is not
    // set here would fail the host's launch, so the hand-off waits for it.
    let missing: Vec<String> = draft
        .connections
        .iter()
        .filter_map(|(_, connection)| match &connection.auth {
            efference::setup::AuthSource::Env { key } => Some(key.clone()),
            efference::setup::AuthSource::CodexCache
            | efference::setup::AuthSource::Keychain
            | efference::setup::AuthSource::Oauth => None,
        })
        .filter(|key| std::env::var_os(key).is_none())
        .collect();
    terminal.say(Tone::Plain, "");
    if !missing.is_empty() {
        terminal.say(
            Tone::Note,
            &format!(
                "{} {} not set in this shell. Export {} (or use direnv, or `op run -- …`), \
                 then run `efference` in the project you want to set up.",
                missing.join(" and "),
                if missing.len() == 1 { "is" } else { "are" },
                if missing.len() == 1 { "it" } else { "them" },
            ),
        );
        return Ok(());
    }
    let here = std::env::current_dir()?;
    if here.join(efference::config::DEFAULT_PATH).exists() {
        // Already set up here: nothing to offer, and starting a session is the
        // ordinary launch, not the setup profile.
        return Ok(());
    }
    let kept = format!("Wrote {}.", path.display());
    offer_here(&mut terminal, Some(kept), bind, display()?).await
}
