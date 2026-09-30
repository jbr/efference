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

/// Where the setup conversation runs, when the person chooses to start it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The TUI, in this terminal.
    Terminal,
    /// The web client, on a loopback page this terminal holds open.
    Browser,
}

/// Ask whether to set up `here` (as the person would read the path) now, and
/// where, naming the history default the session would run on when past picks
/// apply. With no past pick the new-session form names the current model.
/// `None` is a decline, which has already said what to do instead.
pub fn offer_project_setup<P: Prompter>(
    prompter: &mut P,
    here: &str,
    model: Option<&str>,
) -> Result<Option<Surface>, Cancelled> {
    let billed = match model {
        Some(model) => format!("billed as ordinary use of {model}"),
        None => "billed as ordinary model use — you'll pick the model in the form".to_string(),
    };
    prompter.say(
        Tone::Plain,
        &format!(
            "{here} has no efference.toml yet. Setting it up starts an agent session \
             that looks over the project, asks you a few questions, and writes the \
             file — a conversation {billed}."
        ),
    );
    let choices = [
        Choice::new("In this terminal"),
        Choice::new("In the browser").detail("a local page; this terminal keeps it running"),
        Choice::new("Not now"),
    ];
    match prompter.select(&format!("Set up {here} now?"), &choices)? {
        0 => return Ok(Some(Surface::Terminal)),
        1 => return Ok(Some(Surface::Browser)),
        _ => {}
    }
    prompter.say(
        Tone::Plain,
        "Nothing started. Run `efference` in the project you want to set up.",
    );
    Ok(None)
}
