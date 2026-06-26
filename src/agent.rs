//! The agent loop — the turn engine (DESIGN §7).
//!
//! Where the substrate, the transport, and the user meet. Read a user turn, then
//! repeat `project → complete → append assistant → {approve, run, append result}`
//! until the model stops calling tools, then read the next user turn.
//!
//! Every input — including the first turn — arrives over the `Frontend`, which is
//! the daemon's HTTP-fed [`RemoteFrontend`] (DESIGN §10 control-channel): the
//! control socket is the singular authoritative input source. The log lives in the
//! shared daemon state ([`Shared`]); the loop appends through it, which both
//! persists the JSONL line and wakes any SSE tailers.

use std::sync::Arc;

use crate::api::{Client, Error, ToolChoice};
use crate::consolidate;
use crate::daemon::{Shared, StreamDelta};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend};
use crate::log::{
    ContentBlock, Mark, MarkKind, Payload, RawResult, ToolCall, Triage, Turn, WireState,
};
use crate::project::place_trailing_cache_breakpoint;
use crate::tools::Toolbox;

/// A `tool_result` standing in for a call that didn't run (aborted). Every
/// assistant `tool_use` needs exactly one paired result or projection orphans it
/// and the API rejects the request (DESIGN §10).
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
/// rather than as tool output (DESIGN §10).
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
pub async fn run<F: Frontend, D: Distiller>(
    client: &Client,
    frontend: &mut F,
    shared: Arc<Shared>,
    distiller: D,
    toolbox: Toolbox,
) -> Result<(), Error> {
    // World-facing tools plus the harness-level `consolidate` (handled structurally
    // in the loop below, not by a `Provider`). Appended once; the def is a constant
    // `json!`, so `tools[]` stays byte-stable across requests (DESIGN §3).
    let mut tools = toolbox.definitions();
    tools.push(consolidate::definition());
    let mut turn: Turn = 0;

    loop {
        // Read the next user turn (the first one too — it arrives over the control
        // socket now, not argv). `None` (the daemon tearing down) ends the loop.
        eprintln!("\n(awaiting a user turn on the control socket)");
        let next = loop {
            match frontend.read_user_turn().await {
                Some(line) if !line.trim().is_empty() => break Some(line),
                Some(_) => continue, // blank turn: keep waiting.
                None => break None,
            }
        };
        let Some(line) = next else { break };
        turn += 1;
        shared.append(turn, Payload::User(vec![ContentBlock::Text(line)]))?;

        // Inner loop: keep round-tripping while the model calls tools.
        loop {
            // Project, then place the single trailing cache breakpoint just before
            // sending — projection stays pure; the breakpoint walks the tail each
            // request (DESIGN §3).
            let mut messages = shared.project_messages();
            place_trailing_cache_breakpoint(&mut messages);
            // One tool call per turn (DESIGN §8) — the agent picks its next single
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
            // (DESIGN §3). Transport-only — `record_request` never touches the log.
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
                // wire orphans it and the API rejects the request (DESIGN §10).
                if abort_turn || redirect.is_some() {
                    shared.append(turn, aborted(call))?;
                    continue;
                }
                match frontend.approve(call).await {
                    // `consolidate` acts on the log, not the world (DESIGN §10
                    // Phase 3): record the boundary mark + pair the `tool_use`. No
                    // world-run, no distill. Slice 0 — the mark is inert; Slice 1's
                    // projection folds the span it names (its `upto`).
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
                        // `interrupted()` resolves on a `POST /interrupt` — the
                        // tool's kill trigger (kills the process group, DESIGN §6).
                        // Tool output streams to `GET /stream` chunk-by-chunk as it
                        // runs (bash; the MCP tools resolve whole), tagged by call id.
                        let result = toolbox
                            .run(call, frontend.interrupted(), |chunk| {
                                shared.publish_delta(StreamDelta::ToolOutput {
                                    call_id: call.id.0.clone(),
                                    chunk: chunk.to_string(),
                                });
                            })
                            .await;
                        // Distill at append: the canonical wire never carries raw;
                        // `raw` is kept in the log for reversibility (DESIGN §4).
                        // `ForkPeek` re-enters the model here (the §4 fork) — the
                        // prefix is the log through this call's `tool_use`, so we
                        // snapshot it (can't borrow across the distiller's await).
                        let entries = shared.snapshot();
                        let distilled = distiller.distill(client, &entries, call, &result).await;
                        // The fork peek reads the shared canonical prefix (sliding
                        // its TTL), so its usage belongs in the §3 telemetry too.
                        if let Some(u) = distilled.usage {
                            shared.record_request(u);
                        }
                        // Append the result, then the fork verdict as a `Triage` audit
                        // entry keyed by the result's seq (DESIGN §4): the throwaway
                        // peek's *decision* is logged even though its transcript isn't.
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
                                for_result: result_seq,
                                origin: distilled.origin,
                                raw_keep: distilled.raw_keep,
                                inbox: distilled.inbox,
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
                        // course-correct instead of bailing (DESIGN §10).
                        shared.append(turn, rejected(call))?;
                        redirect = Some(message);
                    }
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
