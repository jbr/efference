//! The agent loop — the turn engine (docs/README.md).
//!
//! Where the substrate, the transport, and the user meet. Read a user turn, then
//! repeat `project → complete → append assistant → {approve, run, append result}`
//! until the model stops calling tools, then read the next user turn.
//!
//! Every input — including the first turn — arrives over the `Frontend`, which is
//! the daemon's HTTP-fed [`RemoteFrontend`] (docs/control-channel.md): the
//! control socket is the singular authoritative input source. The log lives in the
//! shared daemon state ([`Shared`]); the loop appends through it, which both
//! persists the JSONL line and wakes any SSE tailers.

use std::sync::Arc;

use serde_json::Value;

use crate::api::{Client, Error, ToolChoice};
use crate::bash;
use crate::consolidate;
use crate::daemon::{Shared, StreamDelta};
use crate::distill::{BgVerdict, Distiller};
use crate::frontend::{Approval, Frontend};
use crate::inbox::{ShownNotification, active_shown};
use crate::policy::{self, Verdict};
use crate::log::{
    ConfigChange, ContentBlock, Disposition, ConfigEdit, EditState, Lifetime, Mark, MarkKind,
    Notification, Payload, RawResult, Spot, Stance, ToolCall, Triage, Turn, WireState,
};
use crate::project::place_trailing_cache_breakpoint;
use crate::spotter::Spotter;
use crate::supervisor;
use crate::tools::Toolbox;

/// The exact `tools[]` a turn advertises: the world tools (`toolbox`) plus the
/// harness tools handled structurally in the loop — `consolidate` (docs/consolidate.md)
/// and the supervisor/inbox tools (docs/supervisor.md). One source of truth so the
/// keepalive ping (docs/substrate.md) and the real turns send a byte-identical prefix;
/// drift here silently breaks the tools cache tier.
pub fn tool_definitions(toolbox: &Toolbox) -> Vec<Value> {
    let mut tools = toolbox.definitions();
    tools.push(consolidate::definition());
    tools.extend(supervisor::tool_definitions());
    tools
}

/// Run a harness supervisor/inbox tool (docs/supervisor.md) against daemon state, or `None`
/// if `call` isn't one (so the caller falls through to the world toolbox). Like
/// `consolidate` these act on `Shared`, not the world — but unlike it their result
/// distills through the universal fork like any tool (no bypass).
fn supervisor_tool_result(shared: &Arc<Shared>, call: &ToolCall) -> Option<RawResult> {
    let handle = || call.input.get("handle").and_then(Value::as_str);
    match call.name.as_str() {
        supervisor::LIST => Some(ok(shared.bg_list_text())),
        supervisor::CHECK_INBOX => Some(ok(shared.inbox_summary())),
        supervisor::READ => Some(match handle() {
            Some(h) => match shared.bg_read_since(h, call.input.get("cursor").and_then(Value::as_u64)) {
                Some(text) => ok(text),
                None => err(format!("no background process {h}")),
            },
            None => err("read_process requires a `handle`".into()),
        }),
        supervisor::KILL => Some(match handle() {
            Some(h) if shared.kill_bg(h) => ok(format!(
                "killed {h} (SIGKILL to its process group); its exit will be confirmed in your inbox."
            )),
            Some(h) => err(format!("no running background process {h}")),
            None => err("kill_process requires a `handle`".into()),
        }),
        _ => None,
    }
}

fn ok(content: String) -> RawResult {
    RawResult { content, is_error: false }
}
fn err(content: String) -> RawResult {
    RawResult { content, is_error: true }
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

/// Enforce [`MAX_TOOL_RESULT_BYTES`]: replace an over-cap result with a hard error
/// (rather than a silent truncation) that names the byte counts and shows a short,
/// char-boundary-safe head, so both the model and the logs see what overflowed and the
/// model knows to narrow the call. Under the cap, the result passes through untouched.
fn cap_tool_result(call: &ToolCall, result: RawResult) -> RawResult {
    let len = result.content.len();
    if len <= MAX_TOOL_RESULT_BYTES {
        return result;
    }
    log::warn!(
        "tool `{}` returned {len} bytes (> {MAX_TOOL_RESULT_BYTES} cap); \
         replacing with a hard error",
        call.name
    );
    let mut head = String::new();
    for ch in result.content.chars() {
        if head.len() + ch.len_utf8() > 1024 {
            break;
        }
        head.push(ch);
    }
    err(format!(
        "tool `{}` returned {len} bytes, over the {MAX_TOOL_RESULT_BYTES}-byte limit \
         for a single tool result. This almost always means the call was too broad \
         (e.g. searching generated or log files). Narrow it — a tighter path, a more \
         specific pattern, or a line range — and try again. First 1KB of the output:\n\n{head}",
        call.name
    ))
}

/// How the idle loop was woken (docs/supervisor.md): a user turn, a blank line (keep idling),
/// a notification event (run a standalone triage), or shutdown.
enum IdleWake {
    Turn(String),
    Blank,
    Event,
    Shutdown,
}

impl IdleWake {
    fn from_turn(line: Option<String>) -> Self {
        match line {
            Some(l) if !l.trim().is_empty() => IdleWake::Turn(l),
            Some(_) => IdleWake::Blank,
            None => IdleWake::Shutdown,
        }
    }
}

/// Coalesce a burst of near-simultaneous events into one idle-triage peek (docs/supervisor.md).
const IDLE_TRIAGE_DEBOUNCE_MS: u64 = 250;

/// The standalone idle triage (docs/supervisor.md — wake-on-notification). Drains the newly
/// arrived notifications (logging them first-show) alongside the still-active ones and
/// the running background tails, runs the tool-result-free triage peek, applies the
/// dispositions, and logs the verdict as a `Triage` with `for_result: None`. Returns
/// **true** iff it pinned something — the escalation contract: a pin grows the wire, so
/// the caller runs a real turn to act on it; nothing pinned leaves the projection
/// byte-identical (audit-only), so the agent returns to idle with its cache still hot.
async fn run_idle_triage<D: Distiller>(
    client: &Client,
    shared: &Arc<Shared>,
    distiller: &D,
    turn: &mut Turn,
) -> Result<bool, Error> {
    // Let a burst settle so several events triage together, not one peek each.
    tokio::time::sleep(std::time::Duration::from_millis(IDLE_TRIAGE_DEBOUNCE_MS)).await;

    let entries = shared.snapshot();
    let mut shown = active_shown(&entries);
    for note in shared.drain_inbox() {
        let source = note.source.clone();
        let body = note.body.clone();
        let seq = shared.append(*turn, Payload::Ambient(note))?;
        shown.push(ShownNotification { seq, source, body });
    }
    let background = shared.bg_snapshot();
    // Nothing to look at (e.g. the waking notifications were dismissed out-of-band before
    // we drained) — back to idle without spending a model round-trip.
    if shown.is_empty() && background.is_empty() {
        return Ok(false);
    }

    let triage = distiller.triage(client, &entries, &shown, &background).await;
    if let Some(u) = triage.usage {
        shared.record_request(u);
    }

    // Apply dispositions; escalate iff anything was pinned (the wire grew, docs/inbox.md).
    let mut inbox = triage.inbox;
    let mut escalate = inbox.iter().any(|(_, d)| matches!(d, Disposition::Pin));
    for (handle, verdict) in triage.background {
        match verdict {
            BgVerdict::Mute => shared.mute_bg(&handle),
            BgVerdict::Pin => {
                if let Some((source, body)) = shared.bg_pin_snapshot(&handle) {
                    let seq = shared.append(
                        *turn,
                        Payload::Ambient(Notification { source, body, slot: Some(handle) }),
                    )?;
                    inbox.push((seq, Disposition::Pin));
                    escalate = true;
                }
            }
        }
    }
    *turn += 1;
    shared.append(
        *turn,
        Payload::Triage(Triage {
            for_result: None,
            origin: triage.origin,
            raw_keep: String::new(),
            inbox,
        }),
    )?;
    Ok(escalate)
}

/// Whether a call is a `bash` invocation asking to be backgrounded (docs/supervisor.md) —
/// `{ "command": …, "background": true }`. Such a call is handled structurally in
/// the loop (it registers a child with the supervisor) rather than run to completion.
fn is_background_bash(call: &ToolCall) -> bool {
    call.name == bash::NAME
        && call
            .input
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// The `command` of a background-bash call, if present.
fn background_command(call: &ToolCall) -> Option<String> {
    call.input.get("command").and_then(Value::as_str).map(str::to_string)
}

/// A `tool_result` standing in for a call that didn't run (aborted). Every
/// assistant `tool_use` needs exactly one paired result or projection orphans it
/// and the API rejects the request (docs/README.md).
fn aborted(call: &ToolCall) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw: RawResult {
            content: "aborted by user".into(),
            is_error: true,
        },
        wire: WireState::Raw,
    }
}

/// A `tool_result` for a call the user rejected in favor of a redirect message.
/// Like [`aborted`] it pairs the orphaned `tool_use`; the human's actual words
/// follow as a separate user turn (the API merges the two consecutive user
/// messages), so the model reads this as context for the steer that follows
/// rather than as tool output (docs/README.md).
fn rejected(call: &ToolCall) -> Payload {
    Payload::ToolResult {
        call_id: call.id.clone(),
        raw: RawResult {
            content: "Not run — the user replied instead of approving; see their next message.".into(),
            is_error: true,
        },
        wire: WireState::Raw,
    }
}

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
    // constant `json!`, so `tools[]` stays byte-stable across requests (docs/substrate.md).
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

        // Inner loop: keep round-tripping while the model calls tools.
        loop {
            // Project, then place the single trailing cache breakpoint just before
            // sending — projection stays pure; the breakpoint walks the tail each
            // request (docs/substrate.md).
            let mut messages = shared.project_messages();
            place_trailing_cache_breakpoint(&mut messages);
            // One tool call per turn (docs/README.md) — the agent picks its next single
            // action; the fork's `retain` is the only forced call. Each streamed
            // prose fragment is published to `GET /stream` as it arrives — the live
            // view; the committed assistant entry on `/events` remains canonical.
            let completion = client
                .complete(&messages, &tools, ToolChoice::auto(), |fragment| {
                    shared.publish_delta(StreamDelta::AssistantText(fragment.to_string()));
                })
                .await?;
            turn += 1;

            // Surface the model's prose, then pull its calls before the payload
            // moves into the log.
            let calls = if let Payload::Assistant { text, calls } = &completion.assistant {
                for ContentBlock::Text(t) in text {
                    if !t.is_empty() {
                        println!("\n{t}");
                    }
                }
                calls.clone()
            } else {
                Vec::new()
            };
            // Record cache/usage telemetry (surfaced on `GET /state`) and print a
            // legible live line: cache read vs creation is the hot/cold signal
            // (docs/substrate.md). Transport-only — `record_request` never touches the log.
            let u = &completion.usage;
            eprintln!(
                "[usage: in {} · out {} · cache read {} · cache write {}]",
                u.input_tokens, u.output_tokens, u.cache_read_input_tokens, u.cache_creation_input_tokens
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
                // The permission cascade (docs/permissions.md). First the
                // deterministic fast-path (backlog/permissions/prefix-fastpath.md): a
                // `Clear` verdict auto-approves with zero model cost. What it can't
                // clear falls to the independent spotter — a fresh-context model rung
                // that re-decides on the residual before the human does, returning a
                // graded stance. Its verdict is logged as a `Spot` audit entry
                // regardless of outcome (the first permission decision that earns a
                // log line; fast-path clears stay unlogged), and its usage reads the
                // shared spotter prefix, so it joins the cache telemetry (docs/substrate.md).
                // Classify against the *effective* policy — base role + the fold of every
                // grant so far (backlog/permissions/config-edit.md). A prior amendment can
                // promote this very call into the fast-path; the spotter sees the same scope.
                let role = shared.effective_config();
                let approval = match policy::classify(&role, call) {
                    Verdict::Clear => Approval::Approve,
                    Verdict::Gate => {
                        let spotting = spotter.assess(&role, call).await;
                        if let Some(u) = spotting.usage {
                            shared.record_request(u);
                        }
                        let stance = spotting.read.stance;
                        shared.append(
                            turn,
                            Payload::Spot(Spot {
                                call_id: call.id.clone(),
                                stance,
                                reason: spotting.read.reason,
                                origin: spotting.origin,
                            }),
                        )?;
                        match stance {
                            // Clear: auto-approve, no human. Watch/Gate both route to
                            // the human gate for now — the watch-window that gives
                            // Watch its own intervention latency is a later unit.
                            Stance::Clear => Approval::Approve,
                            Stance::Watch | Stance::Gate => frontend.approve(call).await,
                        }
                    }
                };
                // A `ConfigEdit` is the orchestrator legislating policy (backlog/permissions/config-edit.md):
                // append the change (which folds into the effective config), then re-classify
                // *this* call against the new policy. The call runs only if the changed rule now
                // clears it — it never rides along unchecked.
                let approval = match approval {
                    Approval::ConfigEdit { change, lifetime, note } => {
                        commit_config_edit(&shared, turn, call, change, lifetime, note)?
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
                            Payload::Mark(Mark { kind: MarkKind::Boundary { upto }, note }),
                        )?;
                        shared.append(
                            turn,
                            Payload::ToolResult {
                                call_id: call.id.clone(),
                                raw: RawResult { content: "consolidated".into(), is_error: false },
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
                        let result = if is_background_bash(call) {
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
                            toolbox
                                .run(call, frontend.interrupted(), |chunk| {
                                    shared.publish_delta(StreamDelta::ToolOutput {
                                        call_id: call.id.0.clone(),
                                        chunk: chunk.to_string(),
                                    });
                                })
                                .await
                        };
                        // Bound the result before it touches anything (defense in depth):
                        // the fork peek, the wire, and the logged `raw`. An over-cap
                        // result becomes a hard error so the model narrows the call.
                        let result = cap_tool_result(call, result);
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
                        // fork prefix `entries` stays byte-identical, docs/substrate.md.)
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
                        // its TTL), so its usage belongs in the docs/substrate.md telemetry too.
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
                            }),
                        )?;
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

/// Legislate a `ConfigEdit` (backlog/permissions/config-edit.md): persist it if asked, append the
/// `Payload::ConfigEdit` (which folds it into the effective policy), then re-classify the gated call
/// against the *new* policy. Returns `Approve` iff the amended rule now clears the call — the
/// human changes the rule; the mechanism, not the gesture, authorizes the run. If the grant
/// doesn't cover this call, it stays gated (a `Redirect` that tells the model why), so nothing
/// rides along unchecked.
///
/// A persist failure degrades the grant to session-only (honest: we log what actually took
/// effect) rather than failing the turn.
fn commit_config_edit(
    shared: &Shared,
    turn: Turn,
    call: &ToolCall,
    change: ConfigChange,
    lifetime: Lifetime,
    note: String,
) -> std::io::Result<Approval> {
    let lifetime = match lifetime {
        Lifetime::Persist => match shared.config_path() {
            Some(path) => match crate::config::persist_config_change(path, &change) {
                Ok(()) => Lifetime::Persist,
                Err(e) => {
                    eprintln!("config edit: could not persist to {}: {e}", path.display());
                    Lifetime::Session
                }
            },
            // The built-in default config has no file to persist to — session-only.
            None => Lifetime::Session,
        },
        Lifetime::Session => Lifetime::Session,
    };
    // The human upgrading a gate is a *direct* commit — the gesture is the commit, no prior
    // proposal to resolve (backlog/permissions/proposal-lifecycle.md).
    shared.append(
        turn,
        Payload::ConfigEdit(ConfigEdit {
            state: EditState::Committed,
            resolves: None,
            change,
            lifetime,
            note,
        }),
    )?;
    let role = shared.effective_config();
    Ok(if policy::classify(&role, call) == Verdict::Clear {
        Approval::Approve
    } else {
        Approval::Redirect(
            "Scope updated — but that grant doesn't cover this exact call, so it stays gated. \
             Adjust the call to fit the new scope and retry."
                .into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::CallId;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: CallId("toolu_x".into()),
            name: name.into(),
            input: serde_json::json!({}),
        }
    }

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            id: CallId("toolu_b".into()),
            name: crate::bash::NAME.into(),
            input: serde_json::json!({ "command": command }),
        }
    }

    /// Legislating a grant that *covers* the gated call amends the policy and approves the
    /// call (the amended rule clears it on re-classify).
    #[test]
    fn legislate_grants_then_approves_a_covered_call() {
        use crate::log::{ConfigChange, Lifetime};
        let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
        let call = bash_call("npm install");
        assert_eq!(policy::classify(&shared.effective_config(), &call), Verdict::Gate);

        let approval = commit_config_edit(
            &shared,
            0,
            &call,
            ConfigChange::Command { command: "npm".into() },
            Lifetime::Session,
            "ok".into(),
        )
        .unwrap();
        assert_eq!(approval, Approval::Approve);
        // The policy is now amended for any future call too.
        assert_eq!(policy::classify(&shared.effective_config(), &call), Verdict::Clear);
    }

    /// The invariant: a grant that does *not* cover the gated call still amends the policy,
    /// but the call does **not** ride along — it's re-checked and stays gated (a redirect).
    #[test]
    fn legislate_does_not_let_an_uncovered_call_ride_along() {
        use crate::log::{ConfigChange, Lifetime};
        let (shared, _rx) = Shared::new(crate::persist::Log::in_memory());
        let call = bash_call("npm install");

        let approval = commit_config_edit(
            &shared,
            0,
            &call,
            ConfigChange::Tool { name: "read".into() }, // unrelated to the bash call
            Lifetime::Session,
            "unrelated".into(),
        )
        .unwrap();
        assert!(matches!(approval, Approval::Redirect(_)), "uncovered call stays gated");
        // The change still landed — legislation happens regardless of the current call.
        assert!(shared.effective_config().tools.contains("read"));
    }

    #[test]
    fn cap_passes_through_results_under_the_limit() {
        let r = ok("a".repeat(MAX_TOOL_RESULT_BYTES));
        let capped = cap_tool_result(&call("search"), r);
        assert!(!capped.is_error);
        assert_eq!(capped.content.len(), MAX_TOOL_RESULT_BYTES);
    }

    #[test]
    fn cap_turns_oversized_results_into_a_hard_error_with_a_head() {
        let huge = "x".repeat(MAX_TOOL_RESULT_BYTES + 1);
        let capped = cap_tool_result(&call("search"), ok(huge));
        assert!(capped.is_error, "over-cap result must be a hard error");
        // The result no longer carries the megabytes; it carries the explanation + head.
        assert!(capped.content.len() < 4096, "the error is small, not the raw payload");
        assert!(capped.content.contains("search"), "names the offending tool");
        assert!(capped.content.contains("over the"), "states it crossed the limit");
    }

    /// The head is cut on a char boundary, never mid-codepoint (it would panic / produce
    /// invalid UTF-8 otherwise).
    #[test]
    fn cap_head_is_char_boundary_safe() {
        // 4-byte chars straddling the 1KB head boundary.
        let huge = "🦀".repeat(MAX_TOOL_RESULT_BYTES); // 4 bytes each → well over the cap
        let capped = cap_tool_result(&call("read"), ok(huge));
        assert!(capped.is_error);
        // Round-trips as valid UTF-8 (the test would not even construct otherwise, but
        // assert the head contains whole crabs).
        assert!(capped.content.contains('🦀'));
    }
}
