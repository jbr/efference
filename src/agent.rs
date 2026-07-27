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
use crate::daemon::{Shared, StreamDelta};
use crate::distill::{BgVerdict, Distiller};
use crate::frontend::{Approval, Frontend};
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
                if !run_idle_triage(client, &shared, &distiller, &mut turn).await? {
                    continue;
                }
                // Escalated: fall into the trajectory with no new user turn — the model
                // responds to the pinned event at the wire tail.
            }
            IdleWake::Turn(line) => {
                turn += 1;
                shared.append(turn, Payload::User(vec![ContentBlock::Text(line)]))?;
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
            for line in frontend.drain_injected_turns() {
                turn += 1;
                shared.append(turn, Payload::User(vec![ContentBlock::Text(line)]))?;
            }
            // Project, then place the single trailing cache breakpoint just before
            // sending — projection stays pure; the breakpoint walks the tail each
            // request (docs/log-and-projection.md).
            let mut messages = shared.project_messages();
            place_trailing_cache_breakpoint(&mut messages);
            // One tool call per turn (docs/README.md) — the agent picks its next single
            // action; the fork's `retain` is the only forced call. Every live signal is
            // published to `GET /stream` as it arrives — prose, thinking, and the opening
            // and arrival of a tool call's input — so a client can show *which phase* the
            // turn is in rather than only its prose (backlog/dogfooding-friction.md). The
            // committed assistant entry on `/events` remains canonical; this is the live view.
            // Supersede the idle line before blocking on the model
            // (backlog/dogfooding-friction.md). It is printed once at the top of the idle loop and
            // is correct at that moment, but nothing replaced it when a submitted turn then
            // entered a phase with no output — so the last thing an operator saw during a long
            // generation was `idle`, which reads as *finished*. The cost of a stale `idle` isn't
            // cosmetic: it invites interrupting or abandoning a live session on false signal.
            eprintln!("(generating — turn {turn})");
            let completion = client
                .complete(&messages, &tools, ToolChoice::auto(), |streamed| {
                    shared.publish_delta(match streamed {
                        Streamed::Text(text) => StreamDelta::AssistantText(text),
                        Streamed::Thinking(text) => StreamDelta::ThinkingText(text),
                        Streamed::ToolStart { name } => StreamDelta::ToolCallStart { name },
                        Streamed::ToolInput(fragment) => StreamDelta::ToolCallInput { fragment },
                    });
                })
                .await?;
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
            shared.record_request(completion.usage.clone());
            shared.append(turn, completion.assistant)?;

            if calls.is_empty() {
                break; // model is done with this turn → wait for the next.
            }

            let mut abort_turn = false;
            let mut redirect: Option<String> = None;
            for call in &calls {
                // Once we've decided to stop running calls (abort or redirect),
                // every remaining `tool_use` still needs a paired result or the
                // wire orphans it and the API rejects the request (docs/README.md).
                if abort_turn || redirect.is_some() {
                    shared.append(turn, aborted(call))?;
                    continue;
                }
                // The interrupt's *before* juncture (docs/control-channel.md): the operator
                // asked out while the model was generating, so this call never runs — the
                // un-run stand-in already says so, and the turn ends here.
                if shared.take_interrupt() {
                    shared.append(turn, aborted(call))?;
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
                let call = match shared.expand_call(call) {
                    Ok(None) => call,
                    Ok(Some(rewritten)) => {
                        expanded = rewritten;
                        &expanded
                    }
                    Err(message) => {
                        shared.append(turn, expansion_error(call, &message))?;
                        continue;
                    }
                };
                // The permission cascade (docs/permissions.md). First the
                // deterministic fast-path (docs/permissions.md): a
                // `Clear` verdict auto-approves with zero model cost. What it can't
                // clear falls to the independent spotter — a fresh-context model rung
                // that re-decides on the residual before the human does, returning a
                // graded stance. Its verdict is logged as a `Spot` audit entry
                // regardless of outcome (the first permission decision that earns a
                // log line; fast-path clears stay unlogged), and its usage reads the
                // shared spotter prefix, so it joins the cache telemetry (docs/log-and-projection.md).
                // Classify against the *effective* policy — base role + the fold of every
                // grant so far (docs/permissions.md). A prior amendment can
                // promote this very call into the fast-path; the spotter sees the same scope.
                let role = shared.effective_config();
                // The session's own config file is its policy *interface*: a write to it is a
                // proposal that must always reach the operator — never auto-cleared by a path grant,
                // never spotter-cleared (docs/permissions.md).
                // It bypasses the whole cascade straight to the human gate; on approval the write
                // lands and fs-watch folds the diff into effective policy.
                let config_write = shared
                    .config_path()
                    .is_some_and(|cfg| policy::is_config_write(call, cfg));
                let approval = if config_write {
                    frontend.approve(call).await
                } else {
                    match policy::classify(&role, call) {
                        Verdict::Clear => Approval::Approve,
                        // HumanOnly (docs/permissions.md): the human takes the
                        // spotter's role *entirely* — skip the spotter (pay no tokens, log no `Spot`)
                        // and gate straight to the human, who both decides this call and grows the
                        // allowlist by hand (`approve-always-at-prefix`, the `.allow` grant). Learning
                        // is relocated to the human, not lost. Autonomous/SpotterHuman fall through to
                        // the spotter rung below.
                        Verdict::Gate if shared.review_level() == ReviewLevel::HumanOnly => {
                            frontend.approve(call).await
                        }
                        Verdict::Gate => {
                            // Prefix-promotion probe (docs/permissions.md): on a
                            // bash move, observe its recurrence and pick the shortest prefix worth
                            // asking the spotter to vouch for *for all arguments*. The spotter answers
                            // via the stance (`clear_always`), so the probe rides the one spot call.
                            let probe = if call.name == bash::NAME {
                                call.input
                                    .get("command")
                                    .and_then(Value::as_str)
                                    .and_then(policy::simple_command_tokens)
                                    .and_then(|tokens| {
                                        shared.observe_and_pick_probe(&role, &tokens)
                                    })
                            } else {
                                None
                            };
                            let spotting = spotter.assess(&role, call, probe.as_deref()).await;
                            if let Some(u) = spotting.usage {
                                shared.record_request(u);
                            }
                            let stance = spotting.read.stance;
                            let reason = spotting.read.reason;
                            let origin = spotting.origin;
                            shared.append(
                                turn,
                                Payload::Spot(Spot {
                                    call_id: call.id.clone(),
                                    stance,
                                    reason: reason.clone(),
                                    origin,
                                    probed_prefix: probe.clone(),
                                }),
                            )?;
                            // The probe's answer *is* the stance: `clear_always` is the safe-vote for
                            // the probed prefix, any other stance is not-safe (a fail-closed fallback
                            // didn't judge, so it casts no vote). When the safe-votes cross the
                            // consensus threshold, file a non-blocking `Proposed(Command)` — inert
                            // until an orchestrator-above commits it; `promotable_command` re-checks it
                            // lexes and isn't already covered. Nothing rides along: the proposal can't
                            // clear anything until committed, and this very call is still decided below.
                            if let Some(prefix) = &probe
                                && origin == Origin::Model
                                && shared.record_probe_vote(prefix, stance == Stance::ClearAlways)
                                && let Some(command) = policy::promotable_command(&role, prefix)
                            {
                                shared.append(
                                    turn,
                                    Payload::ConfigEdit(ConfigEdit {
                                        state: EditState::Proposed,
                                        resolves: None,
                                        change: ConfigChange::Command { command },
                                        note: format!(
                                            "spotter: `{prefix}` judged safe for all arguments — \
                                             proposed for the allowlist"
                                        ),
                                    }),
                                )?;
                            }
                            match stance {
                                // Clear (and its promotion refinement) auto-approve the move, no human.
                                // Watch/Gate route to the human gate (or, autonomous, the redirect) —
                                // the watch-window that gives Watch its own latency is a later unit.
                                Stance::Clear | Stance::ClearAlways => Approval::Approve,
                                // The autonomous flip (docs/permissions.md):
                                // with no synchronous human, don't block on an absent gate — redirect the
                                // model to its config file, the one policy interface it has. (The config
                                // write itself still gates for the orchestrator-above, above.) Supervised:
                                // route to the human gate as before.
                                Stance::Watch | Stance::Gate => {
                                    if shared.autonomous() {
                                        Approval::Redirect(autonomous_gate_message(
                                            shared.config_path(),
                                            &reason,
                                        ))
                                    } else {
                                        frontend.approve(call).await
                                    }
                                }
                            }
                        }
                    }
                };
                // A `ConfigEdit` is the orchestrator legislating policy (docs/permissions.md):
                // append the change (which folds into the effective config), then re-classify
                // *this* call against the new policy. The call runs only if the changed rule now
                // clears it — it never rides along unchecked.
                let approval = match approval {
                    Approval::ConfigEdit { change, note } => {
                        commit_config_edit(&shared, turn, call, change, note)?
                    }
                    other => other,
                };
                match approval {
                    // `consolidate` acts on the log, not the world (docs/consolidate.md):
                    // record the boundary mark + pair the `tool_use`. No world-run, no
                    // distill. Slice 0 — the mark is inert; Slice 1's projection folds
                    // the span it names (its `upto`).
                    Approval::Approve if call.name == consolidate::NAME => {
                        let note = consolidate::summary(&call.input).unwrap_or_default();
                        let upto = consolidate::extent(&shared.snapshot());
                        shared.append(
                            turn,
                            Payload::Mark(Mark {
                                kind: MarkKind::Boundary { upto },
                                note,
                            }),
                        )?;
                        shared.append(
                            turn,
                            Payload::ToolResult {
                                call_id: call.id.clone(),
                                raw: RawResult {
                                    content: "consolidated".into(),
                                    is_error: false,
                                },
                                wire: WireState::Raw,
                            },
                        )?;
                    }
                    Approval::Approve => {
                        // Acquire the result. A backgrounded bash spawn returns
                        // immediately — the child runs on under the supervisor and its
                        // output surfaces at later gates (docs/supervisor.md); everything else
                        // runs to completion through the toolbox. `interrupted()`
                        // resolves on a `POST /interrupt` — the tool's kill trigger
                        // (kills the process group). Tool output streams to `GET
                        // /stream` chunk-by-chunk as it runs (bash; the MCP tools
                        // resolve whole), tagged by call id.
                        // Set only by a foreground→background handoff below: the
                        // operator's optional note, delivered as a user turn after
                        // the handoff result (docs/supervisor.md).
                        let mut handoff_note = None;
                        let result = if bash::is_background_call(call) {
                            match background_command(call) {
                                Some(command) => match shared.spawn_background(command) {
                                    Ok(handle) => RawResult {
                                        content: format!(
                                            "backgrounded as {handle}. its recent output will \
                                             surface in your inbox at each step, and you'll get a \
                                             notification when it exits."
                                        ),
                                        is_error: false,
                                    },
                                    Err(e) => RawResult {
                                        content: format!("failed to background: {e}"),
                                        is_error: true,
                                    },
                                },
                                None => RawResult {
                                    content: "background bash requires a `command`".into(),
                                    is_error: true,
                                },
                            }
                        } else if let Some(r) = supervisor_tool_result(&shared, call) {
                            // A supervisor/inbox read/act tool (docs/supervisor.md) — resolves
                            // against daemon state synchronously, no world run.
                            r
                        } else {
                            if call.name == bash::NAME {
                                // A detach request that never found a running tool
                                // must not label this fresh command's (potential)
                                // handoff — drop any stale note before running.
                                shared.take_detach_note();
                            }
                            let outcome = toolbox
                                .run(call, frontend.interrupted(), |chunk| {
                                    shared.publish_delta(StreamDelta::ToolOutput {
                                        call_id: call.id.0.clone(),
                                        chunk: chunk.to_string(),
                                    });
                                })
                                .await;
                            match outcome {
                                bash::Outcome::Done(result) => result,
                                // The foreground→background handoff (docs/supervisor.md):
                                // the operator detached the running command, so adopt the
                                // live child into the supervisor — seeded with everything
                                // already streamed — and pair the `tool_use` with a
                                // handoff result (`is_error: false`; unlike Ctrl-C, the
                                // command wasn't lost, it moved).
                                bash::Outcome::Detached(detached) => {
                                    let handle = shared.adopt_background(*detached);
                                    handoff_note = shared.take_detach_note();
                                    RawResult {
                                        content: format!(
                                            "the user moved this command to the background as \
                                             {handle}. it is still running: its recent output \
                                             will surface in your inbox at each step, you'll get \
                                             a notification when it exits, and you can \
                                             read_process/kill_process it like any background \
                                             process."
                                        ),
                                        is_error: false,
                                    }
                                }
                            }
                        };
                        // Bound the result before it touches anything (defense in depth):
                        // the fork peek, the wire, and the logged `raw`. An over-cap
                        // result becomes a hard error so the model narrows the call.
                        let result = cap_tool_result(call, result);
                        // A config write is the one call whose *effect* is decided after it runs
                        // (docs/permissions.md): the file must always
                        // parse, so we re-read it here — synchronously, before the result is
                        // logged — and either adopt the edit or revert it. Reverting turns the
                        // result into an error carrying the parse failure, so the author learns
                        // its edit didn't take *in the tool result it's already reading*, rather
                        // than believing an approved write took effect and thrashing against a
                        // wall it thinks it cleared. The fs-watch task calls the same refresh, so
                        // an edit that arrives any other way gets the identical treatment.
                        let result = if config_write {
                            config_write_outcome(&shared, result)
                        } else {
                            result
                        };
                        // The interrupt's *after* juncture (docs/control-channel.md). The
                        // operator asked out while this call ran; it finished (nothing here
                        // can suspend a synchronous tool) but its output stops at the log.
                        // Taken *before* the fork, so the discarded result is never peeked
                        // at, never distilled, and costs no round-trip — the point of the
                        // verb is keeping unwanted output out of the context, and a fork
                        // that read it first would defeat that on the way to the same place.
                        if shared.take_interrupt() {
                            shared.append(turn, withheld(call, result))?;
                            abort_turn = true;
                            continue;
                        }
                        // Distill at append: the canonical wire never carries raw;
                        // `raw` is kept in the log for reversibility (docs/redaction-fork.md).
                        // `ForkPeek` re-enters the model here (the universal triage fork,
                        // docs/redaction-fork.md) — the prefix is the log through this
                        // call's `tool_use`, so we snapshot it (can't borrow across the
                        // distiller's await).
                        let entries = shared.snapshot();
                        // Open the triage gate (docs/inbox.md): re-show the still-active
                        // notifications (left at earlier gates), then log any newly
                        // arrived (ephemeral) ones as first-show `Ambient` entries —
                        // each entry's seq becomes its durable id, the handle the
                        // verdict pins/dismisses by. (They project to nothing, so the
                        // fork prefix `entries` stays byte-identical, docs/log-and-projection.md.)
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
                        let distilled = distiller
                            .distill(client, &entries, call, &result, &shown, &background)
                            .await;
                        // The fork peek reads the shared canonical prefix (sliding
                        // its TTL), so its usage belongs in the docs/log-and-projection.md telemetry too.
                        if let Some(u) = distilled.usage {
                            shared.record_request(u);
                        }
                        // Apply the background verdicts before writing the `Triage`: a
                        // `Pin` snapshots the process's current tail into an `Ambient`
                        // and folds a `Pin` disposition into the inbox verdict — so it
                        // projects at the `Triage` position, reusing the notification
                        // pin path (docs/supervisor.md). A `Mute` just flags the supervisor.
                        let mut inbox = distilled.inbox;
                        for (handle, verdict) in distilled.background {
                            match verdict {
                                BgVerdict::Mute => shared.mute_bg(&handle),
                                BgVerdict::Pin => {
                                    if let Some((source, body)) = shared.bg_pin_snapshot(&handle) {
                                        let seq = shared.append(
                                            turn,
                                            Payload::Ambient(Notification {
                                                source,
                                                body,
                                                slot: Some(handle),
                                            }),
                                        )?;
                                        inbox.push((seq, Disposition::Pin));
                                    }
                                }
                            }
                        }
                        // Append the result, then the fork verdict as a `Triage` audit
                        // entry keyed by the result's seq (docs/redaction-fork.md): the
                        // throwaway peek's *decision* is logged even though its transcript isn't.
                        let result_seq = shared.append(
                            turn,
                            Payload::ToolResult {
                                call_id: call.id.clone(),
                                raw: result,
                                wire: distilled.wire,
                            },
                        )?;
                        shared.append(
                            turn,
                            Payload::Triage(Triage {
                                for_result: Some(result_seq),
                                origin: distilled.origin,
                                raw_keep: distilled.raw_keep,
                                inbox,
                                surfaced: distilled.surfaced,
                            }),
                        )?;
                        // The operator note attached to a handoff lands as a real
                        // user turn right after the handoff's result — direction,
                        // not tool output (docs/supervisor.md). Same canonical
                        // `Payload::User` as the seam-drain; `project` merges it
                        // into the trailing tool_result message.
                        if let Some(note) = handoff_note {
                            turn += 1;
                            shared.append(
                                turn,
                                Payload::User(vec![ContentBlock::Text(format!(
                                    "[on backgrounding the command, the user says:] {note}"
                                ))]),
                            )?;
                        }
                    }
                    Approval::Abort => {
                        shared.append(turn, aborted(call))?;
                        abort_turn = true;
                    }
                    Approval::Redirect(message) => {
                        // Reject this call but keep the turn alive: the human's
                        // message becomes the next user turn so the model can
                        // course-correct instead of bailing (docs/README.md).
                        shared.append(turn, rejected(call))?;
                        redirect = Some(message);
                    }
                    // Resolved into Approve/Redirect by `commit_config_edit` above before this match.
                    Approval::ConfigEdit { .. } => unreachable!("grant is legislated above"),
                }
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
mod triage;

use config_edit::*;
use results::*;
use triage::*;
