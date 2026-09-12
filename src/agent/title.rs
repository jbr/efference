//! The title peek's trigger and run (docs/session-title.md): when a trajectory
//! ends, decide whether to ask the triage model what the session is for, and
//! record the answer.

use super::*;
use crate::log::{LogEntry, Title, TitleAuthor, latest_title};

/// A trajectory count past which a title is re-asked with no user turn in
/// between: an autonomous run's purpose can drift without anyone typing.
pub(super) const TITLE_FALLBACK_TRAJECTORIES: usize = 12;

/// Whether the trajectory that just ended should be followed by a title peek
/// (docs/session-title.md). Yes when no title exists; when a `User` entry landed
/// after the last title — purpose moves with the user's turns, so each is
/// answered once; and when [`TITLE_FALLBACK_TRAJECTORIES`] assistant turns have
/// ended since the last title without one. The one predicate to tune.
pub(super) fn should_solicit_title(entries: &[LogEntry]) -> bool {
    let last_title = entries
        .iter()
        .rposition(|e| matches!(e.payload, Payload::Title(_)));
    let Some(last_title) = last_title else {
        return true;
    };
    let since = &entries[last_title + 1..];
    if since.iter().any(|e| matches!(e.payload, Payload::User(_))) {
        return true;
    }
    // Assistant turns since the last title; every trajectory ends in at least one.
    since
        .iter()
        .filter(|e| matches!(e.payload, Payload::Assistant { .. }))
        .count()
        >= TITLE_FALLBACK_TRAJECTORIES
}

/// Run the title peek over the log as it stands and record the answer as a
/// `Title` entry (docs/session-title.md). A peek with no usable answer records
/// nothing, so the next trajectory end asks again. Never fails the loop: the
/// title is presentation, and a failure here is logged and dropped.
pub(super) async fn solicit_title(
    client: &Client,
    tools: &[Value],
    shared: &Arc<Session>,
    distiller: &ForkPeek,
) {
    let entries = shared.snapshot();
    let current = latest_title(&entries).map(str::to_string);
    let peek = distiller
        .title(client, tools, &entries, current.as_deref())
        .await;
    for u in peek.usage {
        shared.record_request(RequestOrigin::Title, u);
    }
    for why in &peek.retries {
        log::warn!("title peek: {why}");
    }
    let Some(text) = peek.title else {
        return;
    };
    if current.as_deref() != Some(text.as_str()) {
        eprintln!("[title: {text}]");
    }
    if let Err(e) = shared.set_title(Title {
        text,
        author: TitleAuthor::Triage,
    }) {
        log::error!("could not record the title: {e}");
    }
}
