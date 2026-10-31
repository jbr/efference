//! The loop's model request and its retry schedule (docs/transport.md): one request per
//! attempt, each failed attempt kept as a record-only entry, and the stall the schedule
//! ends in when no attempt gives a usable turn.

use super::*;
use crate::api::Completion;

/// What one model request came to.
enum Attempt {
    /// Nothing further to ask: the completion to go on with, or `None` when the operator
    /// ended the generation. Boxed because the other variant is small.
    Settled(Option<Box<Completion>>),
    /// No usable turn. `terminal` carries the stall reason when the provider named the
    /// failure as one no retry can change.
    Failed {
        why: String,
        terminal: Option<StallReason>,
    },
}

/// Ask the model for the next turn, retrying a stream that dies before finalizing and a
/// completion that finalizes with nothing in it (docs/transport.md). Each such attempt
/// commits its content as a record-only entry — the only copy of it anywhere — which
/// projects to nothing, so the next attempt re-sends the same bytes. Without the
/// record-only entry an aborted stream commits as a clean turn, indistinguishable from
/// the agent choosing silence; and a reasoning-only completion would end the turn with
/// nothing said.
///
/// `None` is a trajectory ending without an answer: the operator's abort, a refusal no
/// retry can change, or the budget spent. Clearing the retry-in-flight state is the
/// caller's, on every `Ok` exit.
pub(super) async fn complete_with_retries(
    client: &Client,
    shared: &Session,
    tools: &[Value],
    turns: &[WireTurn],
    toolbox: &mut Toolbox,
    turn: &mut Turn,
) -> Result<Option<Completion>, Error> {
    let mut aborted_attempts: u32 = 0;
    loop {
        // A failed attempt, in either of its two shapes, becomes the same
        // three things: its words, whether the provider named it as one no
        // retry can change, and the stall reason if that ends the trajectory.
        let (why, terminal) = match attempt(client, shared, tools, turns, toolbox, turn).await? {
            Attempt::Settled(completion) => return Ok(completion.map(|boxed| *boxed)),
            Attempt::Failed { why, terminal } => (why, terminal),
        };
        aborted_attempts += 1;
        if let Some(reason) = terminal {
            // The schedule would only repeat the answer, so the trajectory
            // stalls now, with the same audiences as exhaustion — the
            // record-only entry, the log, and the stall topic carrying the
            // provider's words.
            shared.warn(format_args!("upstream refused the request: {why}"));
            shared.set_stall(stalled(
                reason,
                Some(aborted_attempts),
                "the trajectory stopped: the upstream refused the request".to_string(),
                why,
            ));
            return Ok(None);
        }
        let Some(pause) = stream_abort_backoff(aborted_attempts) else {
            shared.warn(format_args!(
                "no usable turn after {aborted_attempts} attempts: {why}"
            ));
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
                     produced no usable turn"
                ),
                why,
            ));
            return Ok(None);
        };
        log::debug!(
            "upstream attempt {aborted_attempts} of {STREAM_ABORT_ATTEMPTS} failed: \
             {why} — retrying"
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
        retry.delay(pause).await;
    }
}

/// Send one request, record what the transport absorbed on the way, and judge the outcome.
async fn attempt(
    client: &Client,
    shared: &Session,
    tools: &[Value],
    turns: &[WireTurn],
    toolbox: &mut Toolbox,
    turn: &mut Turn,
) -> Result<Attempt, Error> {
    // Where a test lands a press between the loop's last juncture and the request
    // arming its abort (`before_request`).
    #[cfg(test)]
    shared.interleave_before_request();
    // The request reads the latch once its abort is armed, so a press since the last
    // juncture stops it before it is sent (docs/control-channel.md).
    let outcome = client
        .with_origin(RequestOrigin::Turn)
        .with_interrupt_latch(shared.interrupt_latch())
        .complete(turns, tools, ToolChoice::auto(), |streamed| {
            shared.publish_delta(stream_delta(streamed));
        })
        .await;
    // Whatever the outcome, the transport may have absorbed one or more
    // failed HTTP attempts on the way here; each gets its own durable
    // record before the outcome is judged (docs/transport.md).
    record_absorbed_attempts(shared, client, turn)?;
    record_image_refusal(shared, client, toolbox, *turn)?;
    match outcome {
        // A finalized completion with neither prose nor a call is asked again
        // over the same bytes, sharing the aborted streams' budget: a plain
        // resend is the one shape every dialect answers (docs/transport.md).
        Ok(mut completion)
            if completion.upstream_failure().is_none() && completion.says_nothing() =>
        {
            completion.exclude_as_empty();
            *turn += 1;
            shared.record_request(RequestOrigin::Turn, completion.request_usage());
            shared.append(*turn, completion.assistant)?;
            Ok(Attempt::Failed {
                why: Exclusion::Empty.why().to_string(),
                terminal: None,
            })
        }
        Ok(completion) => {
            let Some(why) = completion.upstream_failure().map(str::to_string) else {
                return Ok(Attempt::Settled(Some(Box::new(completion))));
            };
            if completion.operator_abort() {
                // The operator ended the generation (docs/control-channel.md).
                // What streamed is kept as the record-only entry an aborted
                // attempt writes, the retry budget is untouched, and the
                // trajectory returns to idle: unlike a killed tool there is
                // no result to continue from. The latch this press set is
                // consumed here, so the next turn's first call is not stopped
                // by it.
                *turn += 1;
                shared.record_request(RequestOrigin::AbortedAttempt, completion.request_usage());
                shared.append(*turn, completion.assistant)?;
                shared.take_interrupt();
                log::debug!("generation ended by the operator");
                return Ok(Attempt::Settled(None));
            }
            *turn += 1;
            shared.record_request(RequestOrigin::AbortedAttempt, completion.request_usage());
            // The provider named the failure as one a retry cannot change
            // (`api/failure.rs`): an exhausted plan, a rejected request.
            let terminal = completion
                .terminal_failure()
                .is_some()
                .then_some(StallReason::UpstreamRefused);
            shared.append(*turn, completion.assistant)?;
            Ok(Attempt::Failed { why, terminal })
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
            *turn += 1;
            shared.append(
                *turn,
                Payload::record_only(
                    AssistantOutput::empty(client.dialect()),
                    Exclusion::Upstream(error.to_string().into()),
                ),
            )?;
            let terminal = (crate::api::error_class(&error) != crate::api::FailureClass::Transient)
                .then_some(StallReason::RequestFailed);
            Ok(Attempt::Failed {
                why: error.to_string(),
                terminal,
            })
        }
    }
}

/// The live delta a streamed piece of the response publishes: every signal reaches the
/// control stream's `output` class as it arrives — prose, thinking, and the opening and
/// arrival of a tool call's input — so a client can show *which output kind* the turn is
/// streaming rather than only its prose (docs/transport.md). The committed assistant entry
/// on `/events` remains canonical; this is the live view.
fn stream_delta(streamed: Streamed) -> StreamDelta {
    match streamed {
        Streamed::Text { index, text } => StreamDelta::AssistantText { index, text },
        Streamed::Thinking { index, text } => StreamDelta::ThinkingText { index, text },
        Streamed::ToolStart { index, id, name } => StreamDelta::ToolCallStart {
            index,
            call_id: id,
            name,
        },
        Streamed::ToolInput { index, fragment } => StreamDelta::ToolCallInput { index, fragment },
    }
}

/// Log a finalized completion and return the calls it made.
///
/// Surfaces the model's prose, then pulls its calls before the payload moves into the log,
/// dialect-neutral through the accessors (docs/log-and-projection.md). A finalized turn's
/// calls parse by construction — the assembler marks a turn with unparseable arguments
/// record-only, and record-only turns never reach here — so an `Err` is a contract breach,
/// not a path. Records the request's cache and usage telemetry (surfaced on `GET /state`):
/// cache read vs creation is the hot/cold signal, and `record_request` never touches the
/// log (docs/log-and-projection.md).
pub(super) fn commit_completion(
    shared: &Session,
    completion: Completion,
    turn: Turn,
) -> Result<Vec<ToolCall>, Error> {
    let output = completion.assistant_output();
    for text in output.text() {
        if !text.is_empty() {
            log::debug!("{text}");
        }
    }
    let calls = output.tool_calls().unwrap_or_else(|e| {
        shared.error(format_args!(
            "finalized turn carried unparseable call arguments: {e}"
        ));
        Vec::new()
    });
    let u = &completion.usage;
    log::debug!(
        "usage: in {} · out {} · cache read {} · cache write {}",
        u.input_tokens,
        u.output_tokens,
        u.cache_read_input_tokens,
        u.cache_creation_input_tokens
    );
    shared.record_request(RequestOrigin::Turn, completion.request_usage());
    let hit_cap = completion.hit_output_cap();
    shared.append(turn, completion.assistant)?;

    // A cap-terminated turn is legible, not silent: whatever arrived
    // before the cap — on a long-reasoning model, often thinking alone —
    // is committed above, and this names why the model went quiet
    // (docs/transport.md). Unannounced, a multi-minute think ending at
    // the cap leaves the session sitting idle, reading as a hang.
    if hit_cap {
        log::debug!("turn ended at the output-token cap");
        shared.notify(Notification::new(
            "turn",
            "The last model turn ended at the output-token cap — what it \
             produced before the cap (possibly only thinking) is committed. \
             Send a turn to continue.",
            Some("max-tokens".to_string()),
        ));
    }
    Ok(calls)
}
