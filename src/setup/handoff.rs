//! The question between the wizard and the agent (docs/operating.md): whether to
//! set efference up for this directory now.
//!
//! Everything before it is free — terminal prompts, a key check, a model
//! listing. What it starts is an agent session, billed as model use, so the
//! question says that before anything is spent. It is asked after the wizard
//! writes the host config and again on every bare `efference` in a directory
//! with no project config, so someone who ran setup in the wrong directory
//! changes directory and runs `efference` again.

use super::*;

/// Ask whether to set up `here` (as the person would read the path) now, naming
/// the model the session would run on when the host config names one. `true`
/// is yes; a decline has already said what to do instead.
pub fn offer_project_setup<P: Prompter>(
    prompter: &mut P,
    here: &str,
    model: Option<&str>,
) -> Result<bool, Cancelled> {
    let billed = match model {
        Some(model) => format!("billed as ordinary use of {model}"),
        None => "billed as ordinary use of your default model".to_string(),
    };
    prompter.say(
        Tone::Plain,
        &format!(
            "{here} has no efference.toml yet. Setting it up starts an agent session \
             that looks over the project, asks you a few questions, and writes the \
             file — a conversation {billed}."
        ),
    );
    if prompter.confirm(&format!("Set up {here} now?"), true)? {
        return Ok(true);
    }
    prompter.say(
        Tone::Plain,
        "Nothing started. Run `efference` in the project you want to set up.",
    );
    Ok(false)
}
