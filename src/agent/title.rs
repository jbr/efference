//! The title peek (docs/session-title.md): ask the triage model, off the cached
//! prefix, for one line saying what the session is for. The title check runs at
//! each turn start, each queued turn delivered mid-task, and trajectory end, and the ask runs as a
//! detached task, so the loop never waits for it.

use super::*;
use crate::record::log::{LogEntry, Title, TitleAuthor, latest_title};
use std::sync::Arc;

/// A trajectory count past which a title is re-asked with no user turn in
/// between: an autonomous run's purpose can drift without anyone typing.
pub(super) const TITLE_FALLBACK_TRAJECTORIES: usize = 12;

/// Whether a title check should be followed by a title peek
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
/// when configured. Clones share one [`Flight`], which is what keeps a session to
/// one ask in flight.
#[derive(Clone)]
pub(super) struct TitleAsk {
    client: Client,
    tools: Vec<Value>,
    triager: Triager,
    flight: Arc<std::sync::Mutex<Flight>>,
}

/// The single-flight state of a session's title asks (docs/session-title.md).
#[derive(Default)]
struct Flight {
    /// An ask task is running.
    running: bool,
    /// A title check said yes while the task ran; the task runs again when its
    /// ask ends, re-checking the predicate.
    rerun: bool,
}

impl TitleAsk {
    pub(super) fn new(client: &Client, tools: &[Value], triager: &Triager) -> Self {
        Self {
            client: client.clone(),
            tools: tools.to_vec(),
            triager: triager.clone(),
            flight: Arc::default(),
        }
    }
}

/// The title check: test the cadence and, when it asks, run the title peek as a
/// detached task (docs/session-title.md). The loop never waits: the task owns the
/// session handle and the request surface, holds while the cache is believed cold the
/// way the idle triage holds, and appends the answer whenever it lands.
///
/// One ask is in flight per session. A title check that passes while one runs
/// marks a rerun instead of spawning: the predicate reads the log, and the in-flight ask
/// has not written its `Title` yet, so a second task would pass the same check
/// and spend a second peek on the same question.
pub(super) fn solicit_title_detached(shared: &Arc<Session>, ask: &TitleAsk) {
    if !should_solicit_title(&shared.snapshot()) {
        return;
    }
    {
        let mut flight = ask.flight.lock().unwrap();
        if flight.running {
            flight.rerun = true;
            return;
        }
        flight.running = true;
    }
    let shared = shared.clone();
    let ask = ask.clone();
    tokio::spawn(async move {
        let mut force = false;
        loop {
            let unseen_user = solicit_title(&shared, &ask, force).await;
            let mut flight = ask.flight.lock().unwrap();
            if !(flight.rerun || unseen_user) {
                flight.running = false;
                return;
            }
            flight.rerun = false;
            force = unseen_user;
        }
    });
}

/// Run the title peek over the log as it stands and record the answer as a
/// `Title` entry (docs/session-title.md). A peek with no usable answer records
/// nothing, so the next title check asks again. Never fails anything: the title is
/// presentation, and a failure here is logged and dropped.
///
/// `force` skips the predicate. Returns whether a `User` entry landed after the
/// log was read for the peek and before its `Title` was appended: the predicate
/// counts that turn as answered, though the peek never saw it, so the caller
/// asks again with `force`.
async fn solicit_title(shared: &Session, ask: &TitleAsk, force: bool) -> bool {
    shared.wait_until_warm().await;
    let entries = shared.snapshot();
    // A title may have landed between the title check and this task's run;
    // re-check rather than spend a redundant peek.
    if !force && !should_solicit_title(&entries) {
        return false;
    }
    let current = latest_title(&entries).map(str::to_string);
    let peek = ask
        .triager
        .title(&ask.client, &ask.tools, &entries, current.as_deref())
        .await;
    for u in peek.usage {
        shared.record_request(RequestOrigin::Title, u);
    }
    for why in &peek.retries {
        log::debug!("title peek: {why}");
    }
    let Some(text) = peek.title else {
        return false;
    };
    if current.as_deref() != Some(text.as_str()) {
        log::debug!("title: {text}");
    }
    let seq = match shared.set_title(Title::new(text, TitleAuthor::Triage)) {
        Ok(Some(seq)) => seq,
        Ok(None) => return false,
        Err(e) => {
            log::error!("could not record the title: {e}");
            return false;
        }
    };
    shared.snapshot()[entries.len()..]
        .iter()
        .any(|e| e.seq < seq && matches!(e.payload, Payload::User(_)))
}
