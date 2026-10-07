//! The agent loop — the turn engine (docs/README.md).
//!
//! Where the log, the transport, and the user meet. Read a user turn, then
//! repeat `project → complete → append assistant → {approve, run, append result}`
//! until the model stops calling tools, then read the next user turn.
//!
//! Every input — including the first turn — arrives over the `Frontend`, which is
//! the daemon's HTTP-fed [`RemoteFrontend`] (docs/control-channel.md): the
//! control socket is the singular authoritative input source. The log lives in the
//! shared daemon state ([`Session`]); the loop appends through it, which both
//! persists the JSONL line and wakes any SSE tailers.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::api::{Client, Error, RetrySurface, Streamed, ToolChoice, UpstreamRetry};
use crate::bash;
use crate::config::Refreshed;
use crate::consolidate;
use crate::daemon::{RequestOrigin, Session, Stall, StallReason, StreamDelta};
use crate::distill::{
    BgVerdict, ForkPeek, Pending, TriagePrimerInput, TriagePrimerOutcome, is_peek_tool,
};
use crate::frontend::{Approval, Frontend, SubmittedTurn};
use crate::inbox::{ShownNotification, active_shown};
use crate::log::{
    AssistantOutput, ConfigChange, ConfigEdit, ContentBlock, Disposition, EditState, Mark,
    Notification, Origin, Payload, RawResult, Spot, Stance, ToolCall, Triage, Turn, WireState,
};
use crate::policy::{self, ReviewLevel, Verdict};
use crate::project::WireTurn;
use crate::spotter::Spotter;
use crate::supervisor;
use crate::tools::Toolbox;

/// Hard cap on a single tool result's bytes (defense in depth). The fork peek caps
/// *line count* (`PEEK_LINE_CAP`), but not a pathological single huge line; an
/// over-cap result is almost always a mistake — e.g. grepping the gitignored session
/// logs, which embed prior raw results, so one matched line can be hundreds of KB —
/// and feeding it onward would bloat the fork peek, blow the request size limit (the
/// 400 this guards against), and, since `raw` is logged verbatim, recursively bloat
/// the next session's searches. Generous, since legitimate large reads should pass;
/// crossing it signals something went wrong, so we don't silently truncate.
const MAX_TOOL_RESULT_BYTES: usize = 256 * 1024;

/// Coalesce a burst of near-simultaneous events into one idle-triage peek (docs/supervisor.md).
const IDLE_TRIAGE_DEBOUNCE_MS: u64 = 250;

/// Attempts per model call when the upstream fails transiently — a stream that
/// dies before finalizing, or a request that could not open one
/// (docs/transport.md): each failed attempt is appended record-only (it projects
/// to nothing, so the replay re-sends byte-identical messages over the still-warm
/// prefix), then the request goes again after a [`stream_abort_backoff`] pause.
/// The fork peek's gate reaches for the same constant and the same schedule
/// (`distill/peek.rs`): one stated policy, whichever surface the failure lands on.
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
/// Both retry loops build their value here, so `turn` and `peek` differ only in
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
/// Each entry carries `upstream_failure`, so it projects to nothing and the next
/// request re-sends byte-identical messages (docs/log-and-projection.md).
fn record_absorbed_attempts(
    shared: &Session,
    client: &Client,
    turn: &mut Turn,
) -> std::io::Result<()> {
    for failure in client.drain_upstream_attempts() {
        *turn += 1;
        shared.append(
            *turn,
            Payload::Assistant {
                output: AssistantOutput::empty(client.dialect()),
                upstream_failure: Some(failure.record_text().into()),
            },
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
    distiller: ForkPeek,
    spotter: S,
    toolbox: Toolbox,
) -> Result<(), Error> {
    // World-facing tools plus the harness tools (`consolidate`, the
    // supervisor/inbox tools (docs/supervisor.md)) handled structurally in the loop
    // below, not by a `Provider`. Built once from the shared source; the defs are
    // constant `json!`, so `tools[]` stays byte-stable across requests (docs/log-and-projection.md).
    let tools = tool_definitions(&toolbox);
    // Continues the log's numbering: `0` for a new session, the last entry's turn for a
    // resumed one, so a resumed session's new turns do not reuse a recorded number.
    let mut turn: Turn = shared.latest_turn();
    // Turns submitted while a resumed session's pending batch waited for the operator's
    // first action (docs/resumption.md). Delivered at the seam after the batch, the way a
    // turn submitted during a live gate is; one still held when the trajectory ends
    // before that seam starts the next.
    let mut held: std::collections::VecDeque<SubmittedTurn> = std::collections::VecDeque::new();

    loop {
        // Idle: race the next user turn against a notification wake (docs/supervisor.md —
        // wake-on-notification, which replaced a blocking `wait`). A user turn drives a
        // normal trajectory; an event wakes a standalone triage that escalates to a
        // trajectory only if it pins something. The first turn arrives here too.
        eprintln!("\n(idle — awaiting a user turn or an event on the control socket)");
        // A resumed session's unfinished batch, derived from the log each time
        // (docs/resumption.md). While it waits, the wire ends in unanswered `tool_use`
        // blocks, so no triage may project it: an event stays in the inbox and rides the
        // batch's own gate.
        let mut replay = shared.pending_batch();
        let next = if let Some(submitted) = replay.is_none().then(|| held.pop_front()).flatten() {
            IdleWake::Turn(submitted)
        } else {
            tokio::select! {
                line = frontend.read_user_turn() => IdleWake::from_turn(line),
                _ = shared.inbox_wake_while_warm(), if replay.is_none() => IdleWake::Event,
                _ = shared.continue_wake() => IdleWake::Continue,
            }
        };
        match next {
            IdleWake::Shutdown => break, // daemon tearing down.
            IdleWake::Blank => continue, // blank turn: keep idling.
            IdleWake::Event => {
                // Standalone triage. Escalate to a trajectory only if it changed the
                // wire (pinned something); otherwise back to idle, the prefix still hot.
                if !run_idle_triage(client, &tools, &shared, &distiller, &mut turn).await? {
                    continue;
                }
                // Escalated: fall into the trajectory with no new user turn — the model
                // responds to the pinned event at the wire tail. No turn was claimed, so
                // nothing else marks the loop running (docs/control-api.md).
                shared.escalated();
            }
            IdleWake::Continue => {
                // The zero-token continue (docs/control-api.md): the operator handed
                // control back with no input, so nothing is appended and nothing on the
                // wire moves — the trajectory resumes at the tail it already had, and
                // the next request re-sends a byte-identical prefix. Consuming the
                // signal here is what keeps one gesture to one extra turn.
                shared.take_continue();
            }
            // The batch comes first: a `User` entry after unanswered `tool_use` blocks
            // breaks the wire, so the turn waits for the seam after it.
            IdleWake::Turn(submitted) if replay.is_some() => held.push_back(submitted),
            IdleWake::Turn(submitted) => {
                turn += 1;
                shared.append_submission(
                    turn,
                    Payload::User(submitted.blocks()),
                    submitted.submission_id,
                    submitted.source,
                )?;
            }
        }

        // A trajectory is starting: the upstream's turn-routing token, if the
        // provider issues one, belongs to the previous trajectory and must not be
        // replayed into this one (docs/transport.md).
        client.begin_turn();
        distiller.begin_turn();

        // A trajectory is starting, so the previous one's give-up is no longer the
        // current condition (docs/transport.md): the `stall` topic names what the
        // daemon is *now*, and a supervisor that answered a stall by sending a turn
        // sees it clear as that turn begins rather than only when it finishes.
        shared.clear_stall();

        // A fresh turn starts un-interrupted: an interrupt fired while the loop was idle has
        // nothing to stop, and a latch left set would silently stop the *next* turn's first
        // call instead (docs/control-channel.md).
        shared.take_interrupt();

        // Inner loop: keep round-tripping while the model calls tools. `first_step` skips the
        // pace sleep on the turn that responds to the just-read user message / escalated event —
        // pacing is a *between-turns* delay, so it applies to the tool round-trips that follow,
        // not the first response (docs/control-channel.md).
        let mut first_step = true;
        loop {
            // Pace (docs/control-channel.md): the operator's between-turns wall-clock
            // dial. An interruptible daemon-side sleep at this same juncture, transparent to the
            // agent (never logged/projected) — a submitted turn or a release ends it immediately,
            // so the drain below picks up anything that arrived during the wait.
            // The config-unparseable park (docs/permissions.md). A config edit
            // resolves to adopted or rejected-with-a-reason; the one outcome that
            // reaches neither is the file failing to parse *and* the restore
            // failing too, which leaves policy in memory that the file no longer
            // shows. Rather than keep running under a perimeter nobody can
            // inspect, the loop stops here until the file parses again — the human
            // fixes it, fs-watch adopts it, and this returns. Checked at every
            // step, not only at the trajectory's start, because an approved config
            // write can land the condition mid-trajectory. Resolves at once in
            // every other case, which is nearly always.
            shared.await_config_repair().await;
            // A resumed session's first action brings its pending batch back
            // (docs/resumption.md): the calls that never ran go through the permission
            // cascade, and everything captured is triaged with them at one gate, with no
            // model request first. The next pass round-trips as after any batch.
            if let Some(pending) = replay.take() {
                first_step = false;
                let Batch {
                    steps,
                    abort_turn,
                    redirect,
                    primer,
                    ..
                } = replay_batch(frontend, &shared, &spotter, &toolbox, &pending).await?;
                land(
                    client, &tools, &shared, &distiller, &mut turn, steps, primer,
                )
                .await?;
                if after_landing(&shared, &mut turn, abort_turn, redirect)? {
                    break;
                }
                continue;
            }
            if !first_step {
                shared.pace_at_seam().await;
            }
            first_step = false;
            // Seam-drain (docs/control-channel.md): deliver any user turns
            // submitted while this trajectory was in flight, at the juncture — after the
            // last tool's results landed, before the next model call. Each lands as a
            // canonical `Payload::User` (never the inbox/triage path — a user turn is
            // undismissable), which `project` merges into the trailing tool_result message
            // as `[tool_result…, text]`. A turn arriving while the loop is idle still takes
            // the fast path (the outer `select!`); this only catches mid-trajectory ones.
            // The cold check (backlog: cold-session.md): no request reads a cache
            // believed expired without the operator's consent. Parks here, a decision
            // point, until `accept_cold_read` arrives; before the drain, so a turn sent
            // with that consent lands at this seam.
            shared.await_warm_or_consent().await;
            let injected = frontend.drain_injected_turns();
            for submitted in held.drain(..).chain(injected) {
                turn += 1;
                // A user turn lands mid-trajectory: to the upstream that is a new
                // turn, so its routing token starts over (docs/transport.md).
                client.begin_turn();
                distiller.begin_turn();
                shared.append_submission(
                    turn,
                    Payload::User(submitted.blocks()),
                    submitted.submission_id,
                    submitted.source,
                )?;
            }
            // Project, then place the single trailing cache breakpoint just before
            // sending — projection stays pure; the breakpoint walks the tail each
            // request (docs/log-and-projection.md).
            let mut turns = shared.project_turns();
            turns.push(WireTurn::Breakpoint);
            // One tool call per turn (docs/README.md) — the agent picks its next single
            // action; the fork's `retain` is the only forced call. Every live signal is
            // published to the control stream's `output` class as it arrives — prose, thinking, and the opening
            // and arrival of a tool call's input — so a client can show *which phase* the
            // turn is in rather than only its prose (docs/transport.md). The
            // committed assistant entry on `/events` remains canonical; this is the live view.
            // Supersede the idle line before blocking on the model
            // (docs/transport.md). It is printed once at the top of the idle loop and
            // is correct at that moment, but nothing replaced it when a submitted turn then
            // entered a phase with no output — so the last thing an operator saw during a long
            // generation was `idle`, which reads as *finished*. The cost of a stale `idle` isn't
            // cosmetic: it invites interrupting or abandoning a live session on false signal.
            eprintln!("(generating — turn {turn})");
            // Bounded retry for a stream that dies before finalizing
            // (docs/transport.md). Each aborted attempt commits its partial as a
            // record-only entry — the only copy of that content anywhere — which
            // projects to nothing, so the next attempt re-sends the same bytes.
            // Without the record-only entry an aborted stream commits as a clean
            // turn — empty or thinking-only, `upstream_failure` null —
            // indistinguishable from the agent choosing silence.
            let mut aborted_attempts: u32 = 0;
            let completion = loop {
                let outcome = client
                    .complete(&turns, &tools, ToolChoice::auto(), |streamed| {
                        shared.publish_delta(match streamed {
                            Streamed::Text { index, text } => {
                                StreamDelta::AssistantText { index, text }
                            }
                            Streamed::Thinking { index, text } => {
                                StreamDelta::ThinkingText { index, text }
                            }
                            Streamed::ToolStart { index, id, name } => StreamDelta::ToolCallStart {
                                index,
                                call_id: id,
                                name,
                            },
                            Streamed::ToolInput { index, fragment } => {
                                StreamDelta::ToolCallInput { index, fragment }
                            }
                        });
                    })
                    .await;
                // Whatever the outcome, the transport may have absorbed one or more
                // failed HTTP attempts on the way here; each gets its own durable
                // record before the outcome is judged (docs/transport.md).
                record_absorbed_attempts(&shared, client, &mut turn)?;
                // A failed attempt, in either of its two shapes, becomes the same
                // three things: its words, whether the provider named it as one no
                // retry can change, and the stall reason if that ends the trajectory.
                let (why, terminal) = match outcome {
                    Ok(completion) => {
                        let Some(why) = completion.upstream_failure().map(str::to_string) else {
                            break Some(completion);
                        };
                        if completion.operator_abort() {
                            // The operator ended the generation (docs/control-channel.md).
                            // What streamed is kept as the record-only entry an aborted
                            // attempt writes, the retry budget is untouched, and the
                            // trajectory returns to idle: unlike a killed tool there is
                            // no result to continue from. The latch this press set is
                            // consumed here, so the next turn's first call is not stopped
                            // by it.
                            turn += 1;
                            shared.record_request(
                                RequestOrigin::AbortedAttempt,
                                completion.request_usage(),
                            );
                            shared.append(turn, completion.assistant)?;
                            shared.take_interrupt();
                            eprintln!("[generation ended by the operator]");
                            break None;
                        }
                        turn += 1;
                        shared.record_request(
                            RequestOrigin::AbortedAttempt,
                            completion.request_usage(),
                        );
                        // The provider named the failure as one a retry cannot change
                        // (`api/failure.rs`): an exhausted plan, a rejected request.
                        let terminal = completion
                            .terminal_failure()
                            .is_some()
                            .then_some(StallReason::UpstreamRefused);
                        shared.append(turn, completion.assistant)?;
                        (why, terminal)
                    }
                    // The request itself failed — nothing streamed, and the transport's
                    // own retry handler has already spent its attempts
                    // (docs/transport.md). An empty record-only entry carries the error
                    // into the log (stderr isn't durable) and the projected prefix is
                    // unchanged. A connection that timed out or was refused, or a
                    // retryable status that outlasted the transport, takes the same
                    // schedule as an aborted stream; a rejection of the request itself
                    // stalls at once.
                    Err(error) => {
                        turn += 1;
                        shared.append(
                            turn,
                            Payload::Assistant {
                                output: AssistantOutput::empty(client.dialect()),
                                upstream_failure: Some(error.to_string().into()),
                            },
                        )?;
                        let terminal = (crate::api::error_class(&error)
                            != crate::api::FailureClass::Transient)
                            .then_some(StallReason::RequestFailed);
                        (error.to_string(), terminal)
                    }
                };
                aborted_attempts += 1;
                if let Some(reason) = terminal {
                    // The schedule would only repeat the answer, so the trajectory
                    // stalls now, with the same audiences as exhaustion — the
                    // record-only entry, stderr, and the stall topic carrying the
                    // provider's words.
                    eprintln!("[upstream refused the request: {why}]");
                    shared.set_stall(stalled(
                        reason,
                        Some(aborted_attempts),
                        "the trajectory stopped: the upstream refused the request".to_string(),
                        why,
                    ));
                    break None;
                }
                let Some(pause) = stream_abort_backoff(aborted_attempts) else {
                    eprintln!("[upstream unusable, {aborted_attempts} attempts exhausted: {why}]");
                    // An upstream failure is told to the operator and the log,
                    // never to the model (docs/transport.md): if we are in a
                    // position to describe the failure to it, we have already
                    // recovered from it, and the description only becomes
                    // something it reasons about mid-task. The topic is
                    // addressed to a supervising *client*, which otherwise sees
                    // an idle daemon indistinguishable from a finished run; it
                    // carries the attempt count and the upstream's own words.
                    shared.set_stall(stalled(
                        StallReason::RetriesExhausted,
                        Some(aborted_attempts),
                        format!(
                            "the trajectory stopped: {aborted_attempts} attempts in a row \
                             failed upstream"
                        ),
                        why,
                    ));
                    break None;
                };
                eprintln!(
                    "[upstream attempt {aborted_attempts} of {STREAM_ABORT_ATTEMPTS} failed: \
                     {why} — retrying]"
                );
                // The wait is the state: publish it before sleeping, so a client can
                // tell "retrying, attempt 3 of 12" from "the model is thinking hard"
                // (docs/transport.md). The operator's continue cuts it short.
                let retry = client.upstream_retry();
                retry.set(stream_abort_retry(
                    RetrySurface::Turn,
                    aborted_attempts,
                    STREAM_ABORT_ATTEMPTS,
                    pause,
                    &why,
                ));
                retry.pause(Some(pause)).await;
            };
            // The all-clear, on every exit from the loop: the attempt that finalized,
            // the exhausted budget (its own visible outcome, via the stall topic and
            // the record-only entries), or a failed request (docs/transport.md).
            client.upstream_retry().clear(RetrySurface::Turn);
            let Some(completion) = completion else {
                break;
            };
            turn += 1;

            // Surface the model's prose, then pull its calls before the payload
            // moves into the log.
            // Dialect-neutral through the accessors (docs/log-and-projection.md). A
            // finalized turn's calls parse by construction — the assembler marks a
            // turn with unparseable arguments record-only, and record-only turns
            // never reach here — so an `Err` is a contract breach, not a path.
            let output = completion.assistant_output();
            for text in output.text() {
                if !text.is_empty() {
                    println!("\n{text}");
                }
            }
            let calls = output.tool_calls().unwrap_or_else(|e| {
                log::error!("finalized turn carried unparseable call arguments: {e}");
                Vec::new()
            });
            // Record cache/usage telemetry (surfaced on `GET /state`) and print a
            // legible live line: cache read vs creation is the hot/cold signal
            // (docs/log-and-projection.md). Transport-only — `record_request` never touches the log.
            let u = &completion.usage;
            eprintln!(
                "[usage: in {} · out {} · cache read {} · cache write {}]",
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens
            );
            shared.record_request(RequestOrigin::Turn, completion.request_usage());
            let hit_cap = completion.stop_reason.as_deref() == Some("max_tokens");
            shared.append(turn, completion.assistant)?;

            // A cap-terminated turn is legible, not silent: whatever arrived
            // before the cap — on a long-reasoning model, often thinking alone —
            // is committed above, and this names why the model went quiet
            // (docs/transport.md). Unannounced, a multi-minute think ending at
            // the cap leaves the session sitting idle, reading as a hang.
            if hit_cap {
                eprintln!("[turn ended at the output-token cap]");
                shared.notify(
                    "turn".to_string(),
                    "The last model turn ended at the output-token cap — what it \
                     produced before the cap (possibly only thinking) is committed. \
                     Send a turn to continue."
                        .to_string(),
                    Some("max-tokens".to_string()),
                    None,
                );
            }

            if calls.is_empty() {
                // The trajectory is ending: the moment the title peek reads the
                // whole of it (docs/session-title.md). Off the prefix this turn
                // just cached, so it costs one warm read; presentation only, so
                // it never blocks or fails the loop.
                if should_solicit_title(&shared.snapshot()) {
                    solicit_title(client, &tools, &shared, &distiller).await;
                }
                break; // model is done with this turn → wait for the next.
            }

            // A completion may carry several calls (docs/README.md). Run them all
            // here, holding the results, and triage them together at one gate below. Triaging between calls would build
            // a fork request whose trailing assistant message still carries unanswered
            // `tool_use` blocks, which the API rejects (docs/redaction-fork.md).
            // The triage-lineage primer replays this completion's input on the own
            // peek's client (backlog/separate-triage-cold-prefix.md). Borrowed, so the
            // primer is byte-identical to the request above; armed only on the own
            // peek, whose lineage the main request never warms. Shared peeks skip it
            // without spending a request.
            let primer = distiller.is_own().then(|| TriagePrimerInput {
                distiller: &distiller,
                turns: &turns,
            });
            let Batch {
                steps,
                abort_turn,
                redirect,
                primer,
                ..
            } = run_batch(frontend, &shared, &spotter, &toolbox, turn, &calls, primer).await?;

            land(
                client, &tools, &shared, &distiller, &mut turn, steps, primer,
            )
            .await?;

            if after_landing(&shared, &mut turn, abort_turn, redirect)? {
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod test;

mod ask;
mod batch;
mod config_edit;
mod gate;
mod landing;
mod replay;
mod results;
mod step;
mod surface;
mod telemetry;
mod title;
mod triage;

use ask::*;
use batch::*;
use config_edit::*;
use gate::*;
use landing::*;
use replay::*;
use results::*;
use step::*;
pub use surface::{brief, callable_names, tool_catalog, tool_definitions};
use title::*;
use triage::*;
