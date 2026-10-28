//! The `[updates]` question (docs/update-check.md): whether a host asks efference.net
//! for a newer version. Asked once, with yes preselected, wherever neither the file nor
//! this run has an answer; the management list's "Update check" entry changes one
//! already given. A file without an answer never checks, so the question is what turns
//! the check on, not a default to find and turn off.

use super::*;

/// What the check does and sends, said before the question.
const EXPLAIN: &str = "efference can ask efference.net once a day whether a newer version \
                       is out, and say so in the terminal and web interfaces. The request \
                       carries only what any web request does (your IP address, and \
                       efference's version), and nothing is installed.";

/// The question, yes preselected.
const QUESTION: &str = "Check for new versions of efference?";

/// Ask, preselecting `current` (yes when there is none), and say what the answer does.
pub(super) fn ask<P: Prompter>(prompter: &mut P, current: Option<bool>) -> Result<bool, Cancelled> {
    prompter.say(Tone::Plain, EXPLAIN);
    let check = prompter.confirm(QUESTION, current.unwrap_or(true))?;
    prompter.say(
        Tone::Good,
        if check {
            "efference will check once a day; `efference setup` turns it off."
        } else {
            "efference will not check; `efference update --check` asks once, when you run it."
        },
    );
    Ok(check)
}

/// Ask unless the file or this run already answered.
pub(super) fn ask_unless_answered<P: Prompter>(
    prompter: &mut P,
    existing: &Existing,
    draft: &mut Draft,
) -> Result<(), Cancelled> {
    if existing.update_check.is_none() && draft.update_check.is_none() {
        draft.update_check = Some(ask(prompter, None)?);
    }
    Ok(())
}

/// The management list's summary of an answer.
pub(super) fn summary(answer: Option<bool>) -> &'static str {
    match answer {
        Some(true) => "once a day",
        Some(false) => "off",
        None => "not answered",
    }
}
