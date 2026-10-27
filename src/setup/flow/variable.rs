//! The "Variable name" prompt, shared by the add path and the keychain reveal.
//! A pasted API key is the usual wrong answer, so the check looks for that shape
//! before it checks the name's grammar, and no refusal repeats what was typed:
//! the text may be a secret.

use super::*;

/// Longest a name can be without an underscore before it reads as a key.
const KEY_LENGTH: usize = 32;

/// Prefixes provider keys are issued with (`sk-` covers `sk-ant-` and `sk-or-`).
const KEY_PREFIXES: [&str; 5] = ["sk-", "xai-", "pplx-", "gsk_", "AIza"];

/// Whether `text` is a portable environment-variable name:
/// `[A-Za-z_][A-Za-z0-9_]*`.
fn is_variable_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `text` reads as an API key: a known provider prefix, or long with no
/// underscore-separated words.
fn looks_like_key(text: &str) -> bool {
    KEY_PREFIXES.iter().any(|prefix| text.starts_with(prefix))
        || (text.len() >= KEY_LENGTH && !text.contains('_'))
}

/// Why `text` is not accepted as a variable name, as a line to say; `None` when
/// it is one. The line never contains `text`.
fn refusal(text: &str) -> Option<&'static str> {
    if looks_like_key(text) {
        Some(
            "That looks like a key, not a variable name. Enter the name of the \
             environment variable that holds it; the key itself is pasted only when asked.",
        )
    } else if !is_variable_name(text) {
        Some("A variable name is letters, digits and '_', and does not start with a digit.")
    } else {
        None
    }
}

/// Ask for the name of the environment variable a key is read from, until the
/// answer is a portable name. An empty answer takes `default` when there is one.
pub(in crate::setup) fn ask_variable_name<P: Prompter>(
    prompter: &mut P,
    default: Option<&str>,
) -> Result<String, Cancelled> {
    loop {
        let typed = prompter.text("Variable name", default)?.trim().to_string();
        match refusal(&typed) {
            Some(line) => prompter.say(Tone::Bad, line),
            None => return Ok(typed),
        }
    }
}
