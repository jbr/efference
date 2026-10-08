//! The title peek (docs/session-title.md): ask the triage model, off the cached
//! prefix, for one line saying what the session is for. The check runs at every
//! gate the turn loop passes and the ask runs as a detached task, so the loop
//! never waits for it.

use super::*;
use crate::log::{LogEntry, Title, TitleAuthor, latest_title};
use std::sync::Arc;

/// A trajectory count past which a title is re-asked with no user turn in
/// between: an autonomous run's purpose can drift without anyone typing.
pub(super) const TITLE_FALLBACK_TRAJECTORIES: usize = 12;

/// Whether the gate that just passed should be followed by a title peek
/// (docs/session-title.md). Yes when no title exists; when a `User` entry landed
/// after the last title — purpose moves with the user's turns, so each is
/// answered once, at the moment it lands; and when [`TITLE_FALLBACK_TRAJECTORIES`]
/// assistant turns have ended since the last title without one. The one
/// predicate to tune.
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

/// The owned request surface a detached title ask carries (docs/session-title.md):
/// clones of the agent turn's client and tools — the shared peek borrows their
/// envelope, so a clone sends the same one — and of the fork's own-peek surface
/// when configured.
#[derive(Clone)]
pub(super) struct TitleAsk {
    client: Client,
    tools: Vec<Value>,
    distiller: ForkPeek,
}

impl TitleAsk {
    pub(super) fn new(client: &Client, tools: &[Value], distiller: &ForkPeek) -> Self {
        Self {
            client: client.clone(),
            tools: tools.to_vec(),
            distiller: distiller.clone(),
        }
    }
}

/// Check the cadence and, when it asks, run the title peek as a detached task
/// (docs/session-title.md). The loop calls this at its gates and never waits:
/// the task owns the session handle and the request surface, holds while the
/// cache is believed cold the way the idle triage holds, and appends the answer
/// whenever it lands. Two asks overlapping are harmless — the last title
/// written wins, and the entry is presentation only.
pub(super) fn solicit_title_detached(shared: &Arc<Session>, ask: &TitleAsk) {
    if !should_solicit_title(&shared.snapshot()) {
        return;
    }
    let shared = shared.clone();
    let ask = ask.clone();
    tokio::spawn(async move {
        solicit_title(&shared, &ask).await;
    });
}

/// Run the title peek over the log as it stands and record the answer as a
/// `Title` entry (docs/session-title.md). A peek with no usable answer records
/// nothing, so the next gate asks again. Never fails anything: the title is
/// presentation, and a failure here is logged and dropped.
async fn solicit_title(shared: &Session, ask: &TitleAsk) {
    shared.wait_until_warm().await;
    let entries = shared.snapshot();
    // A title may have landed between the gate's check and this task's run;
    // re-check rather than spend a redundant peek.
    if !should_solicit_title(&entries) {
        return;
    }
    let current = latest_title(&entries).map(str::to_string);
    let peek = ask
        .distiller
        .title(&ask.client, &ask.tools, &entries, current.as_deref())
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
