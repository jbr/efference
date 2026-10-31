//! The agent loop — the turn engine (docs/README.md).
//!
//! Where the log, the transport, and the user meet. Read a user turn, then
//! repeat `project → complete → append assistant → {approve, run, append result}`
//! until the model stops calling tools, then read the next user turn.
//!
//! Every input — including the first turn — arrives over the `Frontend`, which is
//! the daemon's HTTP-fed [`RemoteFrontend`](crate::daemon::RemoteFrontend)
//! (docs/control-channel.md): the control socket is the singular authoritative input
//! source. The log lives in the shared daemon state ([`Session`]); the loop appends
//! through it, which both persists the JSONL line and wakes any SSE tailers.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::api::{Client, Error, RetrySurface, Streamed, ToolChoice, UpstreamRetry};
use crate::bash;
use crate::config::Refreshed;
use crate::consolidate;
use crate::daemon::Session;
use crate::frontend::{Frontend, GateOptions, SubmittedTurn};
use crate::inbox::ShownNotification;
use crate::log::{
    Approval, AssistantOutput, ConfigChange, ConfigEdit, ContentBlock, Disposition, EditState,
    Exclusion, Notification, Origin, Payload, RawResult, RequestOrigin, Spot, Stance, Summary,
    ToolCall, Triage, Turn, WireState,
};
use crate::policy::{self, ReviewLevel, Verdict};
use crate::process_monitor;
use crate::project::WireTurn;
use crate::protocol::state::{BlockedReason, LoopPhase, Stall, StallReason};
use crate::protocol::stream::StreamDelta;
use crate::spotter::Spotter;
use crate::tools::Toolbox;
use crate::triage::{
    BgVerdict, Pending, TriagePrimerInput, TriagePrimerOutcome, Triager, is_triage_tool,
};

/// Hard cap on a single tool result's bytes (defense in depth). The triage caps
/// *line count* (`TRIAGE_LINE_CAP`), but not a pathological single huge line; an
/// over-cap result is almost always a mistake — e.g. grepping the gitignored session
/// logs, which embed prior raw results, so one matched line can be hundreds of KB —
/// and feeding it onward would bloat the triage, blow the request size limit (the
/// 400 this guards against), and, since `raw` is logged verbatim, recursively bloat
/// the next session's searches. Generous, since legitimate large reads should pass;
/// crossing it signals something went wrong, so we don't silently truncate.
const MAX_TOOL_RESULT_BYTES: usize = 256 * 1024;

/// Coalesce a burst of near-simultaneous events into one idle triage (docs/process-monitor.md).
const IDLE_TRIAGE_DEBOUNCE_MS: u64 = 250;

/// Attempts per model call when the upstream fails transiently — a stream that
/// dies before finalizing, or a request that could not open one
/// (docs/transport.md): each failed attempt is appended record-only (it projects
/// to nothing, so the replay re-sends byte-identical messages over the still-warm
/// prefix), then the request goes again after a [`stream_abort_backoff`] pause.
/// Triage reaches for the same constant and the same schedule
/// (`triage/request.rs`): one stated policy, whichever surface the failure lands on.
///
/// Sized to **wait the failure out**. An upstream failure is usually an ephemeral
/// condition, and the alternative on exhaustion — a stall the operator has to
/// continue — is not clearly better than waiting another few minutes, and is
/// strictly worse when nobody is at the keyboard to read it: exhaustion is
/// legible to a supervising client through the `stall` topic, but the continue
/// still has to come from somewhere (docs/transport.md). Sized against an endpoint whose p99
/// latency sits near the client's timeout, where abort streaks run several deep rather
/// than recovering on the first retry. Twelve attempts on the capped schedule spends
/// about three and a half minutes before giving up.
pub(crate) const STREAM_ABORT_ATTEMPTS: u32 = 12;

/// The pause before the next attempt after `aborted` of them have died, or
/// `None` when the budget is spent. Doubling from one second, **capped** — the
/// cap is what lets the attempt count grow without the wait becoming a hang:
/// uncapped doubling over twelve attempts would sleep for over an hour, with a
/// single pause longer than the whole schedule is now.
pub(crate) fn stream_abort_backoff(aborted: u32) -> Option<std::time::Duration> {
    const CAP: u64 = 30;
    (aborted < STREAM_ABORT_ATTEMPTS)
        .then(|| std::time::Duration::from_secs((1u64 << (aborted - 1).min(20)).min(CAP)))
}

/// The retry-in-flight state one aborted attempt publishes (docs/transport.md).
///
/// Both retry loops build their value here, so `turn` and `triage` differ only in
/// the surface they name — the attempt count, the budget, and the lead line stay
/// the one policy. `next_attempt_at` is absolute, so nothing has to re-emit a
/// countdown; the client subtracts.
pub(crate) fn stream_abort_retry(
    surface: RetrySurface,
    attempt: u32,
    budget: u32,
    pause: std::time::Duration,
    why: &str,
) -> UpstreamRetry {
    UpstreamRetry {
        surface,
        attempt,
        budget,
        next_attempt_at: crate::log::Timestamp::now()
            .0
            .saturating_add(pause.as_millis() as u64),
        message: format!("attempt {attempt} of {budget} failed upstream, retrying"),
        detail: why.to_string(),
    }
}

/// The stalled-trajectory state one give-up publishes (docs/transport.md).
///
/// Both failure paths build their value here, so they differ only in the reason,
/// the attempt count, and the words. `at` is stamped now and is absolute: the
/// topic is level-triggered, and a field counting up from the stall would make it
/// re-emit forever.
pub(crate) fn stalled(
    reason: StallReason,
    attempts: Option<u32>,
    message: String,
    detail: String,
) -> Stall {
    Stall {
        reason,
        attempts,
        at: crate::log::Timestamp::now().0,
        message,
        detail,
    }
}

/// Append one record-only entry per failed HTTP attempt the transport absorbed
/// since the last drain (docs/transport.md).
///
/// The retry handler sits below the loop and cannot append, so the bodies arrive
/// through the client's queue and land here — including the ones a later attempt
/// recovered from, which otherwise leave behind only a status and a count.
/// Each entry is `excluded` for its upstream's failure, so it projects to nothing and the
/// next request re-sends byte-identical messages (docs/log-and-projection.md).
fn record_absorbed_attempts(
    shared: &Session,
    client: &Client,
    turn: &mut Turn,
) -> std::io::Result<()> {
    for failure in client.drain_upstream_attempts() {
        *turn += 1;
        shared.append(
            *turn,
            Payload::record_only(
                AssistantOutput::empty(client.dialect()),
                Exclusion::Upstream(failure.record_text().into()),
            ),
        )?;
    }
    Ok(())
}

/// Drive the loop until input ends. The log lives in [`Session`]; the loop appends
/// through it.
pub async fn run<F: Frontend, S: Spotter>(
    client: &Client,
    frontend: &mut F,
    shared: Arc<Session>,
    triager: Triager,
    spotter: S,
    mut toolbox: Toolbox,
) -> Result<(), Error> {
    restore_image_refusal(&shared, client, &mut toolbox);
    let mut agent = AgentLoop::new(client, frontend, shared, triager, spotter, toolbox);
    loop {
        match agent.idle().await? {
            Idle::Shutdown => return Ok(()),
            Idle::Trajectory { replay } => agent.trajectory(replay).await?,
        }
    }
}

#[cfg(test)]
mod test;

mod ask;
mod batch;
mod config_edit;
mod consolidation;
mod images;
mod landing;
mod replay;
mod request;
mod results;
mod step;
mod surface;
mod telemetry;
mod title;
mod trajectory;
mod triage;
mod triage_hold;

use ask::*;
use batch::*;
use config_edit::*;
use images::*;
use landing::*;
use replay::*;
use request::*;
use results::*;
use step::*;
pub use surface::{
    brief, callable_names, recorded_or_rebuilt_tools, tool_catalog, tool_definitions,
};
use title::*;
use trajectory::{AgentLoop, Idle};
use triage::*;
use triage_hold::*;
