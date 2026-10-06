//! The narrator step of the summary head (docs/summary-head.md): the session's own
//! model reads the trajectory digest and writes the account a fold replaces the
//! earlier conversation with.
//!
//! One request, no fallback. A failure of any kind is returned as an error for the
//! caller to report; this module never substitutes another summary, another model, or
//! an unsummarized read.

use crate::api::{Client, RequestUsage, ToolChoice};
use crate::digest;
use crate::log::{LogEntry, Seq};
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

/// The request's one user turn: the digest of `entries`, with the cut marked when
/// there is one, then the narrator's instructions.
///
/// The digest comes first and the instructions after it, the order the eval narrated
/// in (eval/compaction/README.md).
pub fn request(entries: &[LogEntry], cut: Option<Seq>) -> Result<Vec<WireTurn>, Error> {
    if let Some(cut) = cut {
        let calls = crate::consolidate::split_calls(entries, cut);
        if !calls.is_empty() {
            return Err(Error::SplitsCall { cut, calls });
        }
    }
    let digest = digest::render(
        entries,
        digest::Options {
            cut,
            ..Default::default()
        },
    );
    Ok(vec![WireTurn::text(format!(
        "{}\n\n---\n\n{}",
        digest.trim_end(),
        PROMPT.trim_end()
    ))])
}

/// Write the head for `entries`, narrating up to `cut` (everything, without one).
///
/// `client` and `tools` are the session's own, so the narrator is the session's model
/// under its own system prompt; the account is expected as text, and a tool call in
/// its place is an error.
pub async fn narrate(
    client: &Client,
    tools: &[Value],
    entries: &[LogEntry],
    cut: Option<Seq>,
) -> Result<Narration, Error> {
    let turns = request(entries, cut)?;
    let completion = client
        .complete(&turns, tools, ToolChoice::auto(), |_| {})
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
    if completion.stop_reason.as_deref() == Some("max_tokens") {
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

/// The harness's header above the narrator's account.
pub fn head(account: &str) -> String {
    format!("{}\n\n{}", HEADER.trim_end(), account.trim())
}

#[cfg(test)]
mod test;
