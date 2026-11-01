//! Compaction (docs/summary.md): the session's own model reads the trajectory digest
//! and writes the summary that replaces the earlier conversation.
//!
//! One request, no fallback. A failure of any kind is returned as an error for the
//! caller to report; this module never substitutes another summary, another model, or
//! an unsummarized read.

use crate::curation::digest;
use crate::inference::api::{Client, RequestUsage, Streamed, ToolChoice};
use crate::record::log::{Audience, LogEntry, Payload, Seq};
use crate::record::project::WireTurn;
use serde_json::Value;

const PROMPT: &str = include_str!("../../assets/prompts/compaction.md");
const HEADER: &str = include_str!("../../assets/prompts/summary-header.md");

/// A written summary and the request that wrote it.
#[derive(Debug)]
pub struct Compaction {
    /// The harness's header, then the model's account: the text a compaction's
    /// summary carries.
    pub text: String,
    pub usage: RequestUsage,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The cut would separate a call from its result, which a boundary there could
    /// not project (docs/consolidate.md).
    #[error("a cut at seq {cut} splits {} call(s) from their results", calls.len())]
    SplitsCall {
        cut: Seq,
        calls: Vec<crate::record::log::CallId>,
    },
    #[error("compaction request failed: {0}")]
    Request(crate::inference::api::Error),
    #[error("compaction stream ended early: {0}")]
    Upstream(String),
    #[error("compaction stopped at its output cap before finishing")]
    Truncated,
    #[error("compaction called a tool instead of writing the account")]
    CalledTool,
    #[error("compaction returned no text")]
    Empty,
}

/// The request's one user turn: the accounts earlier compactions left, when there are
/// any; the digest of what came after the latest of them, with the cut marked when
/// there is one; then the instructions.
///
/// The earlier accounts are context, not material: the projection keeps each
/// compaction's text in order (docs/summary.md), so the reader already has them above
/// the new one, and summarizing them again would summarize a summary. The digest comes
/// first and the instructions after it, the order the eval ran in
/// (eval/compaction/README.md).
pub fn request(entries: &[LogEntry], cut: Option<Seq>) -> Result<Vec<WireTurn>, Error> {
    if let Some(cut) = cut {
        let calls = crate::curation::consolidate::split_calls(entries, cut);
        if !calls.is_empty() {
            return Err(Error::SplitsCall { cut, calls });
        }
    }
    let mut earlier = String::new();
    let mut covered = None;
    for entry in entries {
        if let Payload::Summary(summary) = &entry.payload
            && summary.from == 0
            && summary.audience == Audience::All
        {
            earlier.push_str(EARLIER);
            earlier.push_str(account_of(&summary.text));
            earlier.push_str("\n\n");
            covered = Some(covered.map_or(summary.upto, |c: Seq| c.max(summary.upto)));
        }
    }
    let after: Vec<LogEntry> = entries
        .iter()
        .filter(|e| covered.is_none_or(|c| e.seq > c) && !matches!(e.payload, Payload::Summary(_)))
        .cloned()
        .collect();
    let digest = digest::render(
        &after,
        digest::Options {
            cut,
            ..Default::default()
        },
    );
    Ok(vec![WireTurn::text(format!(
        "{earlier}{}\n\n---\n\n{}",
        digest.trim_end(),
        PROMPT.trim_end()
    ))])
}

/// Write the summary for `entries`, covering up to `cut` (everything, without one).
///
/// `client` and `tools` are the session's own, so the writer is the session's model
/// under its own system prompt; the account is expected as text, and a tool call in
/// its place is an error.
///
/// `on_delta` sees the response's fragments as they arrive; a caller with no one to
/// show them to passes `|_| {}`.
pub async fn summarize(
    client: &Client,
    tools: &[Value],
    entries: &[LogEntry],
    cut: Option<Seq>,
    on_delta: impl FnMut(Streamed),
) -> Result<Compaction, Error> {
    let turns = request(entries, cut)?;
    let completion = client
        .with_origin(crate::record::log::RequestOrigin::Compaction)
        .complete(&turns, tools, ToolChoice::auto(), on_delta)
        .await
        .map_err(Error::Request)?;
    if let Some(why) = completion.upstream_failure() {
        return Err(Error::Upstream(why.to_string()));
    }
    account(&completion).map(|account| Compaction {
        text: with_header(&account),
        usage: completion.request_usage(),
    })
}

fn account(completion: &crate::inference::api::Completion) -> Result<String, Error> {
    if completion.hit_output_cap() {
        return Err(Error::Truncated);
    }
    let output = completion.assistant_output();
    if output.tool_calls().is_ok_and(|calls| !calls.is_empty()) {
        return Err(Error::CalledTool);
    }
    let account = output.text().join("\n").trim().to_string();
    if account.is_empty() {
        return Err(Error::Empty);
    }
    Ok(account)
}

/// The heading over an earlier compaction's account in the request.
pub const EARLIER: &str = "### earlier account: already in the reader's context, above yours\n";

/// The harness's header above the model's account.
pub fn with_header(account: &str) -> String {
    format!("{}\n\n{}", HEADER.trim_end(), account.trim())
}

/// A compaction's account without the harness's header: what an earlier compaction
/// told the reader, for the next one to read as context.
pub(crate) fn account_of(text: &str) -> &str {
    text.strip_prefix(HEADER.trim_end()).unwrap_or(text).trim()
}

#[cfg(test)]
mod test;
