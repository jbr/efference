//! The agent loop — Phase 0's turn engine (DESIGN §7).
//!
//! Where the substrate, the transport, and the user meet. Seed a user turn, then
//! repeat `project → complete → append assistant → {approve, run, append result}`
//! until the model stops calling tools, then read the next user turn (EOF quits).
//!
//! The first turn comes from **argv**, not stdin: a backgrounded run can't be fed
//! its opening prompt over stdin (the same agent-usability constraint that gives
//! us the SIGUSR1 approval channel, DESIGN §10). Subsequent turns come over the
//! `Frontend`. The [`Log`] persists every entry to JSONL as it's appended.

use crate::api::{Client, Error, ToolChoice};
use crate::distill::Distiller;
use crate::frontend::{Approval, Frontend};
use crate::log::{ContentBlock, Payload, RawResult, ToolCall, Turn, WireState};
use crate::persist::Log;
use crate::project::{place_trailing_cache_breakpoint, project};
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

/// Drive the loop until the user ends input. Owns the log for the session.
pub async fn run<F: Frontend, D: Distiller>(
    client: &Client,
    frontend: &mut F,
    first_prompt: String,
    mut log: Log,
    distiller: D,
    toolbox: Toolbox,
) -> Result<(), Error> {
    let tools = toolbox.definitions();
    let mut turn: Turn = 0;

    // The first user turn is seeded from argv.
    log.append(turn, Payload::User(vec![ContentBlock::Text(first_prompt)]))?;

    loop {
        // Inner loop: keep round-tripping while the model calls tools.
        loop {
            // Project, then place the single trailing cache breakpoint just
            // before sending — projection stays pure; the breakpoint walks the
            // tail each request (DESIGN §3).
            let mut messages = project(log.entries());
            place_trailing_cache_breakpoint(&mut messages);
            // One tool call per turn (DESIGN §8) — the agent picks its next single
            // action; the fork's `retain` is the only forced call.
            let completion = client
                .complete(&messages, &tools, ToolChoice::auto())
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
            eprintln!("[usage: {:?}]", completion.usage);
            log.append(turn, completion.assistant)?;

            if calls.is_empty() {
                break; // model is done with this turn → ask the user for more.
            }

            let mut abort_turn = false;
            let mut redirect: Option<String> = None;
            for call in &calls {
                // Once we've decided to stop running calls (abort or redirect),
                // every remaining `tool_use` still needs a paired result or the
                // wire orphans it and the API rejects the request (DESIGN §10).
                if abort_turn || redirect.is_some() {
                    log.append(turn, aborted(call))?;
                    continue;
                }
                match frontend.approve(call).await {
                    Approval::Approve => {
                        // `interrupted()` resolves on SIGINT — the tool's kill
                        // trigger (C-c during a bash run `killpg`s its process
                        // group, DESIGN §6). The toolbox routes by name.
                        let result = toolbox.run(call, frontend.interrupted()).await;
                        // Distill at append: the canonical wire never carries raw;
                        // `raw` is kept in the log for reversibility (DESIGN §4).
                        // `ForkPeek` re-enters the model here (the §4 fork) — the
                        // prefix is the log through this call's `tool_use`.
                        let wire = distiller.distill(client, log.entries(), call, &result).await;
                        log.append(
                            turn,
                            Payload::ToolResult {
                                call_id: call.id.clone(),
                                raw: result,
                                wire,
                            },
                        )?;
                    }
                    Approval::Abort => {
                        log.append(turn, aborted(call))?;
                        abort_turn = true;
                    }
                    Approval::Redirect(message) => {
                        // Reject this call but keep the turn alive: the human's
                        // message becomes the next user turn so the model can
                        // course-correct instead of bailing (DESIGN §10).
                        log.append(turn, rejected(call))?;
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
                log.append(turn, Payload::User(vec![ContentBlock::Text(message)]))?;
            }
        }

        // The model is waiting on the user. Read the next turn, skipping blank
        // lines; EOF (Ctrl-D, or a backgrounded run with no stdin) ends it.
        eprintln!("\n(your turn — type a message, Ctrl-D to quit)");
        let next = loop {
            match frontend.read_user_turn().await {
                Some(line) if !line.trim().is_empty() => break Some(line),
                Some(_) => continue, // blank line: re-prompt.
                None => break None,
            }
        };
        match next {
            Some(line) => {
                turn += 1;
                log.append(turn, Payload::User(vec![ContentBlock::Text(line)]))?;
            }
            None => break,
        }
    }

    Ok(())
}
