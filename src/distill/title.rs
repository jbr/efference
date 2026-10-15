//! The title peek (docs/session-title.md): ask the triage model, off the cached
//! prefix, for one line saying what the session is for.
//!
//! A text answer, not a tool: a tool would have to ride the shared `tools[]`
//! (docs/redaction-fork.md), which the agent turn also sends, so the agent would
//! see a schema that is never for it and the envelope pin would move for a job
//! unrelated to results. The ask lives in the discarded tail and the answer is
//! the completion's text; `tool_choice` stays `auto` on both lineages, so the
//! shared peek's envelope still matches the agent turn's.

use super::*;

/// The word budget the ask states and the parse enforces with slack: the title
/// is a sidebar line and a list row — an HTML `<title>`, not a summary. The ask
/// says six words because a model cannot count characters (tokenization) but
/// generates in words, so a word budget is one it can actually hit; the
/// enforced ceiling leaves room for articles and counting fuzz. Titles longer
/// than this are rejected as unusable and the peek retried.
pub const TITLE_MAX_WORDS: usize = 8;

/// The runaway guard under the word budget: one long "word" (a URL) must still
/// be rejected.
pub const TITLE_MAX_CHARS: usize = 120;

/// The ask, appended after the breakpoint. `current` is the standing title, so
/// the model keeps it unless the session's purpose moved — the title should
/// change when the work changes, not when the wording could be improved.
pub(super) fn title_instruction(current: Option<&str>) -> String {
    let mut ask = String::from(
        "In one line, at most six words, say what this session is for — the work as a \
         whole, as its author would answer \"what are you working on?\", not what the \
         last few steps were doing. Reply with the line only: no quotes, no label, no \
         tool call.",
    );
    match current {
        Some(current) => {
            ask.push_str("\n\nThe current title is: ");
            ask.push_str(current);
            ask.push_str(
                "\nKeep it verbatim unless the session's purpose has changed since it was \
                 written, or it is longer than the length above; don't reword a title \
                 that fits.",
            );
        }
        None => ask.push_str("\n\nThere is no title yet."),
    }
    ask
}

/// The renewal sentence a non-compliant title attempt gets.
pub(super) const TITLE_RENEW: &str =
    "Reply with the title now: one line, at most six words, as text.";

/// What a title peek came back with: the accepted line, or `None` after the
/// attempts were spent, and the peek's usage and retry breadcrumbs either way.
pub struct TitlePeek {
    pub title: Option<String>,
    pub usage: Vec<RequestUsage>,
    pub retries: Vec<String>,
}

impl ForkPeek {
    /// Ask for the session's title over `prefix` (docs/session-title.md). Never
    /// fails the caller: an unusable answer after the in-fork retries is `None`.
    pub async fn title(
        &self,
        agent: &Client,
        agent_tools: &[Value],
        prefix: &[LogEntry],
        current: Option<&str>,
    ) -> TitlePeek {
        let mut messages = project_semantic_for(prefix, self.reader());
        messages.push(WireTurn::Breakpoint);
        messages.push(WireTurn::text(title_instruction(current)));
        let mut record = PeekRecord::default();
        let parsed = peek_with_retries(
            &self.client(agent, RequestOrigin::Title),
            &agent.upstream_retry(),
            Patience::ASIDE,
            messages,
            self.tools(agent_tools),
            ToolChoice::auto(),
            TITLE_RENEW,
            parse_title,
            &mut record,
        )
        .await;
        let title = match parsed {
            Ok(title) => Some(title),
            Err(why) => {
                log::debug!("title peek produced no usable line; title unchanged — {why}");
                None
            }
        };
        TitlePeek {
            title,
            usage: record.usage,
            retries: record.retries,
        }
    }
}

/// The title out of a completion: the first non-empty text line, with any
/// wrapping quotes or backticks and a leading `Title:` label removed. A tool call,
/// no text, or a line over [`TITLE_MAX_CHARS`] is unusable — the `Err` is the
/// retry nudge.
pub(super) fn parse_title(completion: &Completion) -> Result<String, String> {
    let output = completion.assistant_output();
    if output.tool_calls().is_ok_and(|calls| !calls.is_empty()) {
        return Err("a tool call was made where a text line was asked for".into());
    }
    let text = output.text().join("\n");
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "no text came back".to_string())?;
    let line = line
        .trim_start_matches(['"', '\'', '`', '*'])
        .trim_end_matches(['"', '\'', '`', '*', '.'])
        .trim();
    let line = line
        .strip_prefix("Title:")
        .or_else(|| line.strip_prefix("title:"))
        .map_or(line, str::trim);
    if line.is_empty() {
        return Err("the line was empty once unwrapped".into());
    }
    if line.split_whitespace().count() > TITLE_MAX_WORDS {
        return Err(format!("the line is over {TITLE_MAX_WORDS} words"));
    }
    if line.chars().count() > TITLE_MAX_CHARS {
        return Err(format!("the line is over {TITLE_MAX_CHARS} characters"));
    }
    Ok(line.to_string())
}
