//! The agent loop — the turn engine (docs/README.md).
//!
//! Where the log, the transport, and the user meet. Read a user turn, then
//! repeat `project → complete → append assistant → {approve, run, append result}`
//! until the model stops calling tools, then read the next user turn.
//!
//! Every input — including the first turn — arrives over the `Frontend`, which is
//! the daemon's HTTP-fed [`RemoteFrontend`] (docs/control-channel.md): the
//! control socket is the singular authoritative input source. The log lives in the
//! shared daemon state ([`Shared`]); the loop appends through it, which both
//! persists the JSONL line and wakes any SSE tailers.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::api::{Client, Error, RetrySurface, Streamed, ToolChoice, UpstreamRetry};
use crate::bash;
use crate::config::Refreshed;
use crate::consolidate;
use crate::daemon::{RequestOrigin, Shared, Stall, StallReason, StreamDelta};
use crate::distill::{BgVerdict, ForkPeek, Pending, is_peek_tool};
use crate::frontend::{Approval, Frontend, SubmittedTurn};
use crate::inbox::{ShownNotification, active_shown};
use crate::log::{
    ConfigChange, ConfigEdit, ContentBlock, Disposition, EditState, Mark, MarkKind, Notification,
    Origin, Payload, RawResult, Spot, Stance, ToolCall, Triage, Turn, WireState,
};
use crate::policy::{self, ReviewLevel, Verdict};
use crate::project::place_trailing_cache_breakpoint;
use crate::spotter::Spotter;
use crate::supervisor;
use crate::tools::Toolbox;

/// The exact `tools[]` a turn advertises: the world tools (`toolbox`) plus the
/// harness tools handled structurally in the loop — `consolidate` (docs/consolidate.md)
/// and the supervisor/inbox tools (docs/supervisor.md). One source of truth so the
/// keepalive ping (docs/log-and-projection.md) and the real turns send a byte-identical prefix;
/// drift here silently breaks the tools cache tier.
pub fn tool_definitions(toolbox: &Toolbox) -> Vec<Value> {
    let mut tools = toolbox.definitions();
    tools.push(consolidate::definition());
    tools.extend(supervisor::tool_definitions());
    // The peek tools (docs/redaction-fork.md) stand in every turn's array even though
    // only a fork peek's tail instruction ever asks for them: with the tools array,
    // `tool_choice`, and thinking policy identical, the fork peek is the same
    // conversation continued with a different tail and reads the agent lineage's cache
    // instead of keying its own (docs/log-and-projection.md). Their schemas are static
    // for the same reason. An un-asked call is paired with a nudge result in the loop.
    tools.push(crate::distill::retain_tool());
    tools.push(crate::distill::triage_tool());
    tools
}

/// The names a turn's `tools[]` carries that a call may actually name — read off
/// [`tool_definitions`] itself, so the advertised array and this list cannot drift.
///
/// The peek tools are excluded: `retain`/`triage` stand in every array for
/// cache-lineage reasons but nothing is ever asked of them on an agent turn, and an
/// un-asked call is short-circuited before this is consulted
/// (docs/redaction-fork.md). What is left is what a call can name and have run.
pub fn callable_names(toolbox: &Toolbox) -> Vec<String> {
    tool_definitions(toolbox)
        .iter()
        .filter_map(|d| d.get("name")?.as_str().map(str::to_string))
        .filter(|name| !crate::distill::is_peek_tool(name))
        .collect()
}

/// The spotter's brief for any tool a turn advertises — the lookup mirror of
/// [`tool_definitions`]: the toolbox providers, then the loop-owned tools
/// (`consolidate`, the supervisor/inbox family). One lookup covering the whole
/// array, so a tool in `tools[]` can't be invisible to the permission rung —
/// the loop-owned family once was, and the spotter judged `check_inbox` and
/// `list_processes` as bare names, calling them opaque (docs/permissions.md).
/// Bash stays brief-less by design (judged as command text), and the peek tools
/// never reach a gate (an un-asked call pairs with a nudge result).
pub fn brief<'a>(toolbox: &'a Toolbox, name: &str) -> Option<crate::tools::ToolBrief<'a>> {
    toolbox
        .brief(name)
        .or_else(|| consolidate::brief(name))
        .or_else(|| supervisor::brief(name))
}

/// The full definition behind every wire name a turn's `tools[]` can carry —
/// the `GET /v1/tools/:name` mirror of [`tool_definitions`], the way [`brief`]
/// already mirrors it for the spotter. Built once, before the loop starts (the
/// toolbox's mounted set and this harness's own tool family are both fixed for
/// the session, docs/log-and-projection.md), and snapshotted into `Shared`
/// ([`crate::daemon::Shared::set_tool_catalog`]).
///
/// A catalog built from `toolbox.tool_catalog()` alone once missed this whole
/// family — `check_inbox`, `list_processes`, `consolidate`, the two peek
/// tools — the same shape of gap [`brief`]'s doc comment records for the
/// spotter (`check_inbox`/`list_processes` once judged as bare names because
/// the lookup consulted only the providers). Composed from the same brief
/// sources `brief` reads, plus the two peek tools, which `brief` excludes
/// (they never reach a gate) but this route does not: `retain`/`triage` still
/// ride every turn's `tools[]`, so a reader destructuring the fork peek's own
/// call wants their schema too.
///
/// None of these five sources inject [`crate::tools::INTENT`] the way a
/// `Toolbox`-mounted tool's wire entry does — [`tool_definitions`] pushes
/// `consolidate::definition()`, `supervisor::tool_definitions()`, and the peek
/// tools onto `toolbox.definitions()` *after* `with_intent` has already run
/// over the toolbox half — so each is served with `inject_intent: false`,
/// matching the schema the model was actually sent.
pub fn tool_catalog(
    toolbox: &Toolbox,
) -> std::collections::HashMap<String, crate::tools::ToolEntry> {
    let mut catalog = toolbox.tool_catalog();

    if let Some(brief) = consolidate::brief(consolidate::NAME) {
        catalog.insert(
            consolidate::NAME.to_string(),
            crate::tools::ToolEntry::from_definition(brief.definition, None, false),
        );
    }
    for name in [
        supervisor::LIST,
        supervisor::READ,
        supervisor::KILL,
        supervisor::WATCH,
        supervisor::CHECK_INBOX,
    ] {
        if let Some(brief) = supervisor::brief(name) {
            catalog.insert(
                name.to_string(),
                crate::tools::ToolEntry::from_definition(brief.definition, None, false),
            );
        }
    }
    for raw in [crate::distill::retain_tool(), crate::distill::triage_tool()] {
        if let Some(name) = raw.get("name").and_then(Value::as_str) {
            catalog.insert(
                name.to_string(),
                crate::tools::ToolEntry::from_raw(name, &raw, None, false),
            );
        }
    }
    catalog
}

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

/// Attempts per model call when the stream dies before finalizing
/// (docs/transport.md): each aborted attempt's partial is appended record-only
/// (it projects to nothing, so the replay re-sends byte-identical messages over
/// the still-warm prefix), then the request goes again after a
/// [`stream_abort_backoff`] pause. The fork peek reaches for the same constant
/// and the same schedule (`distill.rs`): one stated policy for a stream that
/// dies before finalizing, whichever surface it lands on.
///
/// Sized to **wait the failure out**. A mid-stream abort is an ephemeral
/// upstream condition, and the alternative on exhaustion — idling with "send a
/// turn to retry" — is not clearly better than waiting another few minutes, and
/// is strictly worse when nobody is at the keyboard to read it — exhaustion is
/// legible to a supervising client through the `stall` topic, but a turn still
/// has to come from somewhere (docs/transport.md). Sized against an endpoint whose p99
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
    pause: std::time::Duration,
    why: &str,
) -> UpstreamRetry {
    UpstreamRetry {
        surface,
        attempt,
        budget: STREAM_ABORT_ATTEMPTS,
        next_attempt_at: crate::log::Timestamp::now()
            .0
            .saturating_add(pause.as_millis() as u64),
        message: format!("stream aborted, retrying ({attempt} of {STREAM_ABORT_ATTEMPTS})"),
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
    shared: &Shared,
    client: &Client,
    turn: &mut Turn,
) -> std::io::Result<()> {
    for failure in client.drain_upstream_attempts() {
        *turn += 1;
        shared.append(
            *turn,
            Payload::Assistant {
                thinking: Vec::new(),
                text: Vec::new(),
                calls: Vec::new(),
                upstream_failure: Some(failure.record_text().into()),
            },
        )?;
    }
    Ok(())
}

/// Drive the loop until input ends. The log lives in [`Shared`]; the loop appends
/// through it.
pub async fn run<F: Frontend, S: Spotter>(
    client: &Client,
    frontend: &mut F,
    shared: Arc<Shared>,
    distiller: ForkPeek,
    spotter: S,
    toolbox: Toolbox,
) -> Result<(), Error> {
    // World-facing tools plus the harness tools (`consolidate`, the
    // supervisor/inbox tools (docs/supervisor.md)) handled structurally in the loop
    // below, not by a `Provider`. Built once from the shared source; the defs are
    // constant `json!`, so `tools[]` stays byte-stable across requests (docs/log-and-projection.md).
    let tools = tool_definitions(&toolbox);
    let mut turn: Turn = 0;

    loop {
        // Idle: race the next user turn against a notification wake (docs/supervisor.md —
        // wake-on-notification, which replaced a blocking `wait`). A user turn drives a
        // normal trajectory; an event wakes a standalone triage that escalates to a
        // trajectory only if it pins something. The first turn arrives here too.
        eprintln!("\n(idle — awaiting a user turn or an event on the control socket)");
        let next = tokio::select! {
            line = frontend.read_user_turn() => IdleWake::from_turn(line),
            _ = shared.inbox_wake() => IdleWake::Event,
            _ = shared.continue_wake() => IdleWake::Continue,
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
                // responds to the pinned event at the wire tail.
            }
            IdleWake::Continue => {
                // The zero-token continue (docs/control-api.md): the operator handed
                // control back with no input, so nothing is appended and nothing on the
                // wire moves — the trajectory resumes at the tail it already had, and
                // the next request re-sends a byte-identical prefix. Consuming the
                // signal here is what keeps one gesture to one extra turn.
                shared.take_continue();
            }
            IdleWake::Turn(submitted) => {
                turn += 1;
                shared.append_submission(
                    turn,
                    Payload::User(vec![ContentBlock::Text(submitted.text)]),
                    submitted.submission_id,
                )?;
            }
        }

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
            for submitted in frontend.drain_injected_turns() {
                turn += 1;
                shared.append_submission(
                    turn,
                    Payload::User(vec![ContentBlock::Text(submitted.text)]),
                    submitted.submission_id,
                )?;
            }
            // Project, then place the single trailing cache breakpoint just before
            // sending — projection stays pure; the breakpoint walks the tail each
            // request (docs/log-and-projection.md).
            let mut messages = shared.project_messages();
            place_trailing_cache_breakpoint(&mut messages, client.breakpoint_placement());
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
                    .complete(&messages, &tools, ToolChoice::auto(), |streamed| {
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
                match outcome {
                    Ok(completion) => {
                        let Some(why) = completion.upstream_failure().map(str::to_string) else {
                            break Some(completion);
                        };
                        aborted_attempts += 1;
                        turn += 1;
                        shared.record_request(
                            RequestOrigin::AbortedAttempt,
                            completion.request_usage(),
                        );
                        shared.append(turn, completion.assistant)?;
                        if aborted_attempts >= STREAM_ABORT_ATTEMPTS {
                            eprintln!(
                                "[stream unusable, {aborted_attempts} attempts \
                                 exhausted: {why}]"
                            );
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
                                    "the trajectory stopped: {aborted_attempts} \
                                     attempts in a row aborted upstream — send a \
                                     turn to retry"
                                ),
                                why.clone(),
                            ));
                            break None;
                        }
                        eprintln!(
                            "[stream unusable (attempt {aborted_attempts} of \
                             {STREAM_ABORT_ATTEMPTS}): {why} — retrying]"
                        );
                        if let Some(pause) = stream_abort_backoff(aborted_attempts) {
                            // The wait is the state: publish it before sleeping, so a
                            // client can tell "retrying, attempt 3 of 12" from "the
                            // model is thinking hard" (docs/transport.md).
                            client.upstream_retry().set(stream_abort_retry(
                                RetrySurface::Turn,
                                aborted_attempts,
                                pause,
                                &why,
                            ));
                            tokio::time::sleep(pause).await;
                        }
                    }
                    // The request itself failed — nothing streamed, and the transport's
                    // own retry handler already exhausted the transient family
                    // (docs/transport.md). An empty record-only entry carries the error
                    // body into the log (stderr isn't durable), the projected prefix is
                    // unchanged, and the trajectory fails back to idle with the daemon
                    // and its control socket alive.
                    Err(error) => {
                        eprintln!("[completion failed: {error}]");
                        turn += 1;
                        shared.append(
                            turn,
                            Payload::Assistant {
                                thinking: Vec::new(),
                                text: Vec::new(),
                                calls: Vec::new(),
                                upstream_failure: Some(error.to_string().into()),
                            },
                        )?;
                        // Operator and log, not the model (docs/transport.md): the error
                        // body rides the stall's `detail` and the record-only entry.
                        shared.set_stall(stalled(
                            StallReason::RequestFailed,
                            None,
                            "the trajectory stopped: the upstream request failed — \
                             send a turn to retry"
                                .to_string(),
                            error.to_string(),
                        ));
                        break None;
                    }
                }
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
            let calls = if let Payload::Assistant { text, calls, .. } = &completion.assistant {
                for block in text {
                    if let ContentBlock::Text(t) = block
                        && !t.is_empty()
                    {
                        println!("\n{t}");
                    }
                }
                calls.clone()
            } else {
                Vec::new()
            };
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
                break; // model is done with this turn → wait for the next.
            }

            // A completion usually carries one call (docs/README.md), but
            // `disable_parallel_tool_use` is a *request*, not a guarantee — no
            // OpenRouter-routed endpoint supports the equivalent knob at all, so a
            // provider may return several. Run them all here, holding the results, and
            // triage them together at one gate below. Triaging between calls would build
            // a fork request whose trailing assistant message still carries unanswered
            // `tool_use` blocks, which the API rejects (docs/redaction-fork.md).
            let mut abort_turn = false;
            let mut redirect: Option<String> = None;
            let mut steps: Vec<Step> = Vec::new();
            for call in &calls {
                // Once we've decided to stop running calls (abort or redirect),
                // every remaining `tool_use` still needs a paired result or the
                // wire orphans it and the API rejects the request (docs/README.md).
                if abort_turn || redirect.is_some() {
                    steps.push(Step::direct(aborted(call)));
                    continue;
                }
                // The interrupt's *before* juncture (docs/control-channel.md): the operator
                // asked out while the model was generating, so this call never runs — the
                // un-run stand-in already says so, and the turn ends here.
                if shared.take_interrupt() {
                    steps.push(Step::direct(aborted(call)));
                    abort_turn = true;
                    continue;
                }
                // Path env-var expansion (docs/tools.md): resolve `$TMPDIR` in the
                // call's path fields at one choke point, before anything judges or
                // runs the call — the fast-path, the spotter, and the executor all
                // see the same expanded literal (an unexpanded variable is opaque
                // to the spotter, so this is what makes the path judgeable). A
                // failed expansion (unlisted/undefined name) pairs the call with an
                // error result and never reaches the cascade — a hard stop, not
                // expand-to-empty. The logged `tool_use` keeps the variable form
                // the model wrote; only the in-flight call is rewritten.
                let expanded;
                let running = match shared.expand_call(call) {
                    Ok(None) => call,
                    Ok(Some(rewritten)) => {
                        expanded = rewritten;
                        &expanded
                    }
                    Err(message) => {
                        steps.push(Step::direct(expansion_error(call, &message)));
                        continue;
                    }
                };
                match run_call(frontend, &shared, &spotter, &toolbox, turn, running).await? {
                    Decision::Ran {
                        result,
                        telemetry,
                        handoff,
                    } => steps.push(Step::Ran {
                        call: call.clone(),
                        result,
                        telemetry,
                        handoff,
                    }),
                    Decision::Direct(payloads) => steps.push(Step::Direct(payloads)),
                    Decision::Withheld(payload) => {
                        // The interrupt's *after* juncture (docs/control-channel.md). The
                        // call finished but its output stops at the log — taken before the
                        // gate, so the discarded result is never peeked at and costs no
                        // round-trip, which is the point of the verb.
                        steps.push(Step::Direct(vec![payload]));
                        abort_turn = true;
                    }
                    Decision::Abort(payload) => {
                        steps.push(Step::Direct(vec![payload]));
                        abort_turn = true;
                    }
                    Decision::Redirect { payload, message } => {
                        steps.push(Step::Direct(vec![payload]));
                        redirect = Some(message);
                    }
                }
            }

            // The gate (docs/redaction-fork.md): one peek over every result this
            // completion produced. Nothing to triage means no round-trip.
            let pending: Vec<Pending<'_>> = steps
                .iter()
                .filter_map(|s| match s {
                    Step::Ran { call, result, .. } => Some(Pending { call, raw: result }),
                    Step::Direct(_) => None,
                })
                .collect();
            let mut gate = if pending.is_empty() {
                None
            } else {
                // `ForkPeek` re-enters the model here — the prefix is the log through
                // this completion's `tool_use` blocks, so we snapshot it (can't borrow
                // across the distiller's await).
                let entries = shared.snapshot();
                // Open the triage gate (docs/inbox.md): re-show the still-active
                // notifications (left at earlier gates), then log any newly arrived
                // (ephemeral) ones as first-show `Ambient` entries — each entry's seq
                // becomes its durable id, the handle the verdict pins/dismisses by. (They
                // project to nothing, so the fork prefix `entries` stays byte-identical,
                // docs/log-and-projection.md.)
                let mut shown = active_shown(&entries);
                for note in shared.drain_inbox() {
                    let source = note.source.clone();
                    let body = note.body.clone();
                    let seq = shared.append(turn, Payload::Ambient(note))?;
                    shown.push(ShownNotification { seq, source, body });
                }
                // The live background processes' fresh tails ride the same peek
                // (docs/supervisor.md) — re-sampled now, never logged unless pinned.
                let background = shared.bg_snapshot();
                Some(
                    distiller
                        .distill(client, &tools, &entries, &pending, &shown, &background)
                        .await,
                )
            };
            drop(pending);

            // The gate's usage: each fork request reads the shared canonical prefix
            // (sliding its TTL), so it belongs in the docs/log-and-projection.md telemetry.
            if let Some(g) = &gate {
                for u in &g.usage {
                    shared.record_request(RequestOrigin::Peek, u.clone());
                }
            }
            // Apply the background verdicts before writing the `Triage`s: a `Pin`
            // snapshots the process's current tail into an `Ambient` and folds a `Pin`
            // disposition into the inbox verdict — so it projects at the `Triage`
            // position, reusing the notification pin path (docs/supervisor.md). A `Mute`
            // just flags the supervisor.
            let mut inbox_verdict = gate
                .as_mut()
                .map(|g| std::mem::take(&mut g.inbox))
                .unwrap_or_default();
            if let Some(g) = &gate {
                for (handle, verdict) in &g.background {
                    match verdict {
                        BgVerdict::Mute => shared.mute_bg(handle),
                        BgVerdict::Pin => {
                            if let Some((source, body)) = shared.bg_pin_snapshot(handle) {
                                let seq = shared.append(
                                    turn,
                                    Payload::Ambient(Notification {
                                        source,
                                        body,
                                        slot: Some(handle.clone()),
                                    }),
                                )?;
                                inbox_verdict.push((seq, Disposition::Pin));
                            }
                        }
                    }
                }
            }

            // Append in call order: each step's own payloads, and for a triaged result
            // its `ToolResult` followed by the `Triage` audit entry keyed to that
            // result's seq (docs/redaction-fork.md). The gate-scoped riders — inbox
            // dispositions and surfaced context — ride the **last** `Triage` of the batch:
            // they were decided once for the whole step, and tail-ward placement keeps the
            // wire append-only (docs/inbox.md), which is exactly where they already land
            // when a completion carried a single call.
            let mut verdicts = gate
                .as_mut()
                .map(|g| std::mem::take(&mut g.results))
                .unwrap_or_default()
                .into_iter();
            let surfaced = gate
                .as_ref()
                .map(|g| g.surfaced.clone())
                .unwrap_or_default();
            // Gate-scoped like the inbox riders: the non-compliant-attempt breadcrumbs
            // land on the last `Triage` of the batch — durable even when a retry
            // recovered (docs/redaction-fork.md).
            let mut retries = gate
                .as_mut()
                .map(|g| std::mem::take(&mut g.retries))
                .unwrap_or_default();
            // Gate-scoped for the same reason: one peek reasoned about the whole batch,
            // so its thinking rides the last `Triage` rather than being copied per
            // result (docs/redaction-fork.md).
            let mut thinking = gate
                .as_mut()
                .map(|g| std::mem::take(&mut g.thinking))
                .unwrap_or_default();
            let last_ran = steps
                .iter()
                .rposition(|s| matches!(s, Step::Ran { .. }))
                .unwrap_or(0);
            let mut handoffs: Vec<String> = Vec::new();
            for (index, step) in steps.into_iter().enumerate() {
                match step {
                    Step::Direct(payloads) => {
                        for p in payloads {
                            shared.append(turn, p)?;
                        }
                    }
                    Step::Ran {
                        call,
                        result,
                        telemetry,
                        handoff,
                    } => {
                        let Some(verdict) = verdicts.next() else {
                            continue;
                        };
                        let result_seq = shared.append(
                            turn,
                            Payload::ToolResult {
                                call_id: call.id.clone(),
                                raw: result,
                                wire: verdict.wire,
                            },
                        )?;
                        let last = index == last_ran;
                        shared.append(
                            turn,
                            Payload::Triage(Triage {
                                for_result: Some(result_seq),
                                origin: verdict.origin,
                                raw_keep: verdict.raw_keep,
                                inbox: if last {
                                    std::mem::take(&mut inbox_verdict)
                                } else {
                                    Vec::new()
                                },
                                surfaced: if last { surfaced.clone() } else { Vec::new() },
                                retries: if last {
                                    std::mem::take(&mut retries)
                                } else {
                                    Vec::new()
                                },
                                thinking: if last {
                                    std::mem::take(&mut thinking)
                                } else {
                                    Vec::new()
                                },
                            }),
                        )?;
                        // The call's telemetry, after the verdict that describes
                        // the same result (docs/tools.md). It projects to nothing,
                        // so appending it cannot move the wire — which is what
                        // lets it be written unconditionally.
                        shared.append(turn, Payload::ToolTelemetry(*telemetry))?;
                        if let Some(note) = handoff {
                            handoffs.push(note);
                        }
                    }
                }
            }
            // The operator note attached to a handoff lands as a real user turn after the
            // step's results — direction, not tool output (docs/supervisor.md). Same
            // canonical `Payload::User` as the seam-drain; `project` merges it into the
            // trailing tool_result message.
            for note in handoffs {
                turn += 1;
                shared.append(
                    turn,
                    Payload::User(vec![ContentBlock::Text(format!(
                        "[on backgrounding the command, the user says:] {note}"
                    ))]),
                )?;
            }

            if abort_turn {
                eprintln!("[turn aborted]");
                break;
            }
            if let Some(message) = redirect {
                // The rejection `tool_result`(s) answer the orphaned call(s); this
                // user turn carries the steer. Don't break — fall through to the
                // inner loop so we re-project and let the model respond.
                turn += 1;
                shared.append(turn, Payload::User(vec![ContentBlock::Text(message)]))?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod test;

mod config_edit;
mod results;
mod step;
mod telemetry;
mod triage;

use config_edit::*;
use results::*;
use step::*;
use triage::*;
