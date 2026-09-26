//! `efference setup` — the interactive first-run path (docs/operating.md), and
//! what a bare `efference` does: setup when there is no configuration; the offer
//! to set this directory up when there is but the directory has no project
//! config and no sessions; otherwise `efference tui`.

use super::*;
use efference::setup::{Existing, LiveProbe, Prompter as _, Tone, terminal::Terminal};

/// The user configuration path under this process's `HOME`.
fn default_config_path() -> Result<PathBuf, CliError> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set; pass --config <path>")?;
    Ok(PathBuf::from(home).join(efference::inference::CONFIG_RELATIVE_PATH))
}

/// Bare `efference`: setup on a machine with no configuration. Configured, a
/// directory with neither a project config nor sessions is offered setup; any
/// other is `efference tui` with nothing named — pick a session here, or with
/// no host running, start one for this terminal.
pub(crate) async fn default_verb() -> CliResult {
    let path = default_config_path()?;
    if !path.exists() {
        return run(Some(path)).await;
    }
    let here = std::env::current_dir()?;
    let (theme, images) = display_env()?;
    let configured = here.join(efference::config::DEFAULT_PATH).exists();
    if !configured {
        let listings = picker::live_listings().await?;
        if picker::menu(&listings, &here).rows.is_empty() {
            return offer_here(&path, theme, images).await;
        }
    }
    picker::run(theme, images, false).await
}

/// What `tui`'s display flags read from the environment, for the bare verb,
/// which has no flags of its own.
fn display_env() -> Result<(Option<String>, efference::tui::ImageMode), CliError> {
    let images = match std::env::var("EFFERENCE_IMAGES") {
        Ok(value) => <ImagesArg as clap::ValueEnum>::from_str(&value, true)
            .map_err(|error| format!("EFFERENCE_IMAGES: {error}"))?,
        Err(_) => ImagesArg::default(),
    };
    Ok((std::env::var("EFFERENCE_THEME").ok(), images.into()))
}

/// Ask whether to set this directory up now, and on yes create a session here —
/// on the running host, or on an island — whose missing project config makes it
/// the setup profile (docs/operating.md).
async fn offer_here(
    config: &Path,
    theme: Option<String>,
    images: efference::tui::ImageMode,
) -> CliResult {
    let here = std::env::current_dir()?;
    // The default model is named in the question because it is what the
    // conversation is billed on. Read leniently: a file the host cannot load
    // fails at launch with its own message.
    let model = std::fs::read_to_string(config)
        .ok()
        .and_then(|text| Existing::from_toml(&text).ok())
        .and_then(|existing| existing.default_model);
    let yes = {
        let mut terminal = Terminal::open()?;
        // Esc at the question is a decline like any other.
        efference::setup::offer_project_setup(
            &mut terminal,
            &picker::tilde(&here),
            model.as_deref(),
        )
        .unwrap_or_default()
    };
    if !yes {
        return Ok(());
    }
    match cli::live_host() {
        Some(host) => tui_new(host, None, SessionFlags::default(), theme, images, false).await,
        None => island::run(None, SessionFlags::default(), theme, images, false).await,
    }
}

/// The verb: ask, render, show, write.
pub(crate) async fn run(config: Option<PathBuf>) -> CliResult {
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
    let (theme, images) = display_env()?;
    offer_here(&path, theme, images).await
}
