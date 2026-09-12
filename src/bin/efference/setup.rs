//! `efference setup` — the interactive first-run path (docs/operating.md), and
//! what a bare `efference` does: setup when there is no configuration, a
//! pointer to the next verb when there is.

use super::*;
use efference::setup::{Existing, LiveProbe, Prompter as _, Tone, terminal::Terminal};

/// The user configuration path under this process's `HOME`.
fn default_config_path() -> Result<PathBuf, CliError> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set; pass --config <path>")?;
    Ok(PathBuf::from(home).join(efference::inference::CONFIG_RELATIVE_PATH))
}

/// Bare `efference`: setup on a machine with no configuration, otherwise the
/// two commands that start a session.
pub(crate) async fn default_verb() -> CliResult {
    let path = default_config_path()?;
    if !path.exists() {
        return run(Some(path)).await;
    }
    eprintln!(
        "efference is configured ({}). Start a host with `efference host`, then \
         `efference tui --new` in a project directory. `efference setup` adds a \
         connection; `efference --help` lists the verbs.",
        path.display()
    );
    Ok(())
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
    let existing = Existing::from_toml(&existing_text)
        .map_err(|error| format!("{} is not TOML: {error}", path.display()))?;
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
    let needs_env: Vec<String> = draft
        .connections
        .iter()
        .filter_map(|(_, connection)| match &connection.auth {
            efference::setup::AuthSource::Env { key } => Some(key.clone()),
            efference::setup::AuthSource::CodexCache | efference::setup::AuthSource::Keychain => {
                None
            }
        })
        .collect();
    terminal.say(Tone::Plain, "");
    terminal.say(Tone::Plain, "Next:");
    if !needs_env.is_empty() {
        terminal.say(
            Tone::Plain,
            &format!(
                "  1. Make sure {} {} in the environment you start the host from \
                 (an exported variable, direnv, or `op run -- …`).",
                needs_env.join(" and "),
                if needs_env.len() == 1 { "is" } else { "are" }
            ),
        );
    }
    let step = if needs_env.is_empty() { 1 } else { 2 };
    terminal.say(
        Tone::Plain,
        &format!("  {step}. `efference host` in a terminal you can leave running."),
    );
    terminal.say(
        Tone::Plain,
        &format!(
            "  {}. `efference tui --new` in a project directory.",
            step + 1
        ),
    );
    Ok(())
}
