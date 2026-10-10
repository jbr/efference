//! The narrator step of the summary head (docs/summary-head.md): the session's own
//! model reads the trajectory digest and writes the account a fold replaces the
//! earlier conversation with.
//!
//! One request, no fallback. A failure of any kind is returned as an error for the
//! caller to report; this module never substitutes another summary, another model, or
//! an unsummarized read.

use crate::api::{Client, RequestUsage, Streamed, ToolChoice};
use crate::digest;
use crate::log::{Audience, LogEntry, Mark, MarkKind, Payload, Seq};
use crate::project::WireTurn;
use serde_json::Value;

const PROMPT: &str = include_str!("../assets/prompts/narrator.md");
const HEADER: &str = include_str!("../assets/prompts/summary-header.md");

/// A written head and the request that wrote it.
#[derive(Debug)]
pub struct Narration {
    /// The harness's header, then the narrator's account: the note a fold's mark
    /// carries.
    pub head: String,
    pub usage: RequestUsage,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The cut would separate a call from its result, which a boundary there could
    /// not project (docs/consolidate.md).
    #[error("a cut at seq {cut} splits {} call(s) from their results", calls.len())]
    SplitsCall {
        cut: Seq,
        calls: Vec<crate::log::CallId>,
    },
    #[error("narrator request failed: {0}")]
    Request(crate::api::Error),
    #[error("narrator stream ended early: {0}")]
    Upstream(String),
    #[error("narrator stopped at its output cap before finishing")]
    Truncated,
    #[error("narrator called a tool instead of writing the account")]
    CalledTool,
    #[error("narrator returned no text")]
    Empty,
}

/// The request's one user turn: the accounts earlier folds left, when there are any;
/// the digest of what came after the latest of them, with the cut marked when there is
/// one; then the narrator's instructions.
///
/// The earlier accounts are context, not material: the projection keeps each fold's
/// account in order (docs/summary-head.md), so the reader already has them above the
/// new one, and narrating them again would summarize a summary. The digest comes first
/// and the instructions after it, the order the eval narrated in
/// (eval/compaction/README.md).
pub fn request(entries: &[LogEntry], cut: Option<Seq>) -> Result<Vec<WireTurn>, Error> {
    if let Some(cut) = cut {
        let calls = crate::consolidate::split_calls(entries, cut);
        if !calls.is_empty() {
            return Err(Error::SplitsCall { cut, calls });
        }
    }
    let mut earlier = String::new();
    let mut covered = None;
    for entry in entries {
        if let Payload::Mark(Mark {
            kind: MarkKind::Boundary { upto },
            note,
            audience: Audience::All,
            ..
        }) = &entry.payload
        {
            earlier.push_str(EARLIER);
            earlier.push_str(account_of(note));
            earlier.push_str("\n\n");
            covered = Some(covered.map_or(*upto, |c: Seq| c.max(*upto)));
        }
    }
    let after: Vec<LogEntry> = entries
        .iter()
        .filter(|e| covered.is_none_or(|c| e.seq > c) && !matches!(e.payload, Payload::Mark(_)))
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

/// Write the head for `entries`, narrating up to `cut` (everything, without one).
///
/// `client` and `tools` are the session's own, so the narrator is the session's model
/// under its own system prompt; the account is expected as text, and a tool call in
/// its place is an error.
///
/// `on_delta` sees the response's fragments as they arrive; a caller with no one to
/// show them to passes `|_| {}`.
pub async fn narrate(
    client: &Client,
    tools: &[Value],
    entries: &[LogEntry],
    cut: Option<Seq>,
    on_delta: impl FnMut(Streamed),
) -> Result<Narration, Error> {
    let turns = request(entries, cut)?;
    let completion = client
        .with_origin(crate::log::RequestOrigin::Narrate)
        .complete(&turns, tools, ToolChoice::auto(), on_delta)
        .await
        .map_err(Error::Request)?;
    if let Some(why) = completion.upstream_failure() {
        return Err(Error::Upstream(why.to_string()));
    }
    account(&completion).map(|account| Narration {
        head: head(&account),
        usage: completion.request_usage(),
    })
}

fn account(completion: &crate::api::Completion) -> Result<String, Error> {
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

/// The heading over an earlier fold's account in the narrator's request.
pub const EARLIER: &str = "### earlier account: already in the reader's context, above yours\n";

/// The harness's header above the narrator's account.
pub fn head(account: &str) -> String {
    format!("{}\n\n{}", HEADER.trim_end(), account.trim())
}

/// A head's account without the harness's header: what an earlier fold told the
/// reader, for the next narrator to read as context.
fn account_of(note: &str) -> &str {
    note.strip_prefix(HEADER.trim_end()).unwrap_or(note).trim()
}

#[cfg(test)]
mod test;
