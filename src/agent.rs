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

use crate::api::{Client, Error, Streamed, ToolChoice};
use crate::bash;
use crate::config::Refreshed;
use crate::consolidate;
use crate::daemon::{RequestOrigin, Shared, StreamDelta};
use crate::distill::{BgVerdict, Distiller, Pending, is_peek_tool};
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

/// Drive the loop until input ends. The log lives in [`Shared`]; the loop appends
/// through it.
pub async fn run<F: Frontend, D: Distiller, S: Spotter>(
    client: &Client,
    frontend: &mut F,
    shared: Arc<Shared>,
    distiller: D,
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
            IdleWake::Turn(submitted) => {
                turn += 1;
                shared.append_submission(
                    turn,
                    Payload::User(vec![ContentBlock::Text(submitted.text)]),
                    submitted.submission_id,
                )?;
            }
        }

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
            place_trailing_cache_breakpoint(&mut messages);
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
            let completion = match client
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
                .await
            {
                Ok(completion) => completion,
                // No assistant payload exists to append, so the projected prefix remains
                // unchanged. Treat an exhausted upstream request as a failed turn and
                // return to the outer idle loop with the daemon and its control socket alive.
                Err(error) => {
                    eprintln!("[completion failed: {error}]");
                    break;
                }
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
            // (docs/transport.md). Observed live before this existed: a
            // multi-minute think ended at the cap and the session just sat
            // idle, reading as a hang.
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
                    Decision::Ran { result, handoff } => steps.push(Step::Ran {
                        call: call.clone(),
                        result,
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
                            }),
                        )?;
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
mod triage;

use config_edit::*;
use results::*;
use step::*;
use triage::*;
