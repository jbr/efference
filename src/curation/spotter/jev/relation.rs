//! The relation request: how the move relates to earlier calls the operator
//! approved (docs/permissions.md).
//!
//! The cold request judges the move alone. This one is sent beside it when the
//! session has consented calls of the move's family, and asks one `choice`
//! question per candidate with that call inlined in the question's instructions.
//! The model favors the first option it is shown, so each candidate is asked in
//! both option orders (`rel_<i>` and `relr_<i>`) and a reading is the mean of the
//! two.
//!
//! The move and the candidates are plaintext rather than the cold request's
//! structured `move`: a bash command is the command as typed, any other tool is
//! its name and one `key: value` line per argument, so the two sides of the
//! comparison read the same way.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::inference::api::{Decisions, Instructions, Question};
use crate::record::log::{Compared, ToolCall};
use crate::record::names::tools::{BASH, INTENT, display_name};

use super::questions::{RELATION_COUNTS, Reading};

/// How many consented same-family calls are asked about, most recent first.
const CANDIDATES_MAX: usize = 10;

const QUESTION: &str = "How does the move in the state relate to this earlier call?";

/// The relation options in the order they are first offered. The second ask
/// reverses it.
const OPTIONS: [(&str, &str); 3] = [
    (
        "same",
        "The same action on the same kind of target: it differs only in which file, line range, search term, element, or record it reads or edits, in the same place the earlier call worked. A narrower version of the earlier call also counts as the same.",
    ),
    (
        "wider",
        "It goes further than the earlier call: it changes or deletes where the earlier call only read, sends where the earlier call only fetched, or reaches a different host, account, service, or a directory outside the place the earlier call worked.",
    ),
    ("different", "A different kind of action altogether."),
];

/// What a candidate's text is prefixed with. Every candidate is a consented call
/// (`precedent::consented`), so every one carries it.
const APPROVED_LABEL: &str = "[OPERATOR_APPROVED] ";

/// The key of a candidate's question in the given option order.
pub(super) fn key(index: usize, reversed: bool) -> String {
    match reversed {
        false => format!("rel_{index}"),
        true => format!("relr_{index}"),
    }
}

/// The call's family: the same tool, or for bash the same program after a leading
/// `cd … &&` and any `NAME=value` words. It only chooses what to compare; the
/// model does the comparing.
pub(super) fn family(call: &ToolCall) -> String {
    let Some(command) = bash_command(call) else {
        return display_name(&call.name);
    };
    let program = without_leading_cd(command.trim())
        .split_whitespace()
        .find(|word| !is_assignment(word))
        .map(|word| word.rsplit('/').next().unwrap_or(word))
        .filter(|name| !name.is_empty())
        .unwrap_or("?");
    format!("{BASH}:{program}")
}

fn bash_command(call: &ToolCall) -> Option<&str> {
    (call.name == BASH).then(|| {
        call.input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
    })
}

/// `cd <dir> && ` or `cd <dir> ; ` at the start, removed.
fn without_leading_cd(command: &str) -> &str {
    let Some(rest) = command.strip_prefix("cd ") else {
        return command;
    };
    let Some((dir, rest)) = rest.split_once(' ') else {
        return command;
    };
    if dir.is_empty() || dir.contains(char::is_whitespace) {
        return command;
    }
    match rest.strip_prefix("&&").or_else(|| rest.strip_prefix(';')) {
        Some(rest) => rest.trim_start_matches(' '),
        None => command,
    }
}

/// A word shaped `NAME=…`.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A call as plaintext: a bash command raw, continuation lines indented two
/// spaces; any other tool as its display name and one `  key: value` line per
/// argument (non-string values JSON-encoded, continuation lines indented four).
/// The harness-injected `intent` is withheld, as in the cold request's state
/// (docs/permissions.md).
pub(super) fn plaintext(call: &ToolCall) -> String {
    if let Some(command) = bash_command(call) {
        return format!("$ {}", command.replace('\n', "\n  "));
    }
    let mut text = display_name(&call.name);
    if let Some(arguments) = call.input.as_object() {
        for (name, value) in arguments.iter().filter(|(name, _)| *name != INTENT) {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            text.push_str(&format!("\n  {name}: {}", value.replace('\n', "\n    ")));
        }
    }
    text
}

/// The consented calls of the move's family, the `CANDIDATES_MAX` most recent,
/// oldest first.
pub(super) fn candidates<'a>(call: &ToolCall, consented: &'a [ToolCall]) -> Vec<&'a ToolCall> {
    let family = family(call);
    let same: Vec<&ToolCall> = consented
        .iter()
        .filter(|c| self::family(c) == family)
        .collect();
    same[same.len().saturating_sub(CANDIDATES_MAX)..].to_vec()
}

/// The relation request's state: the move and where it runs.
pub(super) fn state(call: &ToolCall, cwd: Option<&Path>) -> Value {
    json!({
        "move": plaintext(call),
        "working_directory": cwd.map(|p| p.display().to_string()),
    })
}

/// The questions for these candidates: each asked in both option orders.
pub(super) fn questions(candidates: &[&ToolCall]) -> BTreeMap<String, Question> {
    let mut questions = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let text = format!("{APPROVED_LABEL}{}", plaintext(candidate));
        for reversed in [false, true] {
            let options: Vec<(&str, &str)> = match reversed {
                false => OPTIONS.to_vec(),
                true => OPTIONS.iter().rev().copied().collect(),
            };
            questions.insert(
                key(index, reversed),
                Question::choice(
                    Instructions::fields([("question", QUESTION), ("earlier_call", text.as_str())]),
                    options,
                ),
            );
        }
    }
    questions
}

/// The mean of an option's probability across the candidate's two askings, or
/// zero when either went unanswered: a candidate the model did not read twice is
/// not evidence.
fn mean(decisions: &Decisions, index: usize, option: &str) -> f64 {
    let read = |reversed| {
        decisions
            .probabilities(&key(index, reversed))
            .and_then(|p| p.get(option).copied())
    };
    match (read(false), read(true)) {
        (Some(a), Some(b)) => (a + b) / 2.0,
        _ => 0.0,
    }
}

/// Each candidate's figures, in the order asked, for the record.
pub(super) fn compared(decisions: &Decisions, candidates: &[&ToolCall]) -> Vec<Compared> {
    candidates
        .iter()
        .enumerate()
        .map(|(i, call)| Compared {
            call_id: call.id.clone(),
            same: mean(decisions, i, "same"),
            wider: mean(decisions, i, "wider"),
        })
        .collect()
}

/// Count the candidates read as the `same` action and as `wider`.
pub(super) fn read(decisions: &Decisions, candidates: usize) -> Reading {
    let counted = |option: &str| {
        (0..candidates)
            .filter(|&i| mean(decisions, i, option) >= RELATION_COUNTS)
            .count()
    };
    Reading {
        candidates,
        same: counted("same"),
        wider: counted("wider"),
    }
}

#[cfg(test)]
mod test;
