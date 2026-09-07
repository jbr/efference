//! Round-trips against the model with bounded in-fork retries (docs/redaction-fork.md).

use super::*;

/// Fork round-trips per gate: the ask plus bounded in-fork retries
/// (docs/redaction-fork.md). Each retry re-reads the warm prefix and appends only to
/// the throwaway tail; exhaustion withholds the output as a hard error, which the
/// agent may answer by re-issuing the call — so retrying here is cheaper than making
/// the agent recover.
pub(super) const PEEK_ATTEMPTS: usize = 3;

/// What one gate accumulates across its attempts, whatever the verdict turns out to be:
/// the billing record, the compliance breadcrumbs, and the peek's reasoning
/// (docs/redaction-fork.md). The three travel together because every attempt appends to
/// all of them and the outcome carries all of them onto the gate's `Triage`.
#[derive(Default)]
pub(super) struct PeekRecord {
    /// One [`RequestUsage`] per request the gate made, in-fork retries and aborted
    /// streams included — the request happened and slid the prefix's cache TTL.
    pub(super) usage: Vec<RequestUsage>,
    /// One attempt-stamped breadcrumb per attempt that had to be retried, recovered or
    /// not.
    pub(super) retries: Vec<String>,
    /// The thinking text of each attempt that thought, in attempt order.
    pub(super) thinking: Vec<String>,
}

/// Run one peek to a usable verdict, with bounded in-fork retries
/// (docs/redaction-fork.md). `tool` is the asked-for tool (`retain` or `triage`);
/// `parse` decodes a usable verdict out of a completion, or says why it couldn't —
/// the `Err` doubles as the retry nudge and, on exhaustion, the failure breadcrumb.
/// Every request's [`RequestUsage`] is pushed onto `record.usage` whether or not its
/// verdict was usable (the request happened and slid the prefix's TTL; only a transport error
/// yields none).
///
/// A usable response carrying an unusable verdict — the wrong tool, no tool, a
/// malformed or incomplete input — extends the throwaway tail and goes again: the
/// non-compliant assistant turn is appended verbatim (thinking blocks and all), each
/// of its calls is answered with an error `tool_result` carrying the why, and the ask
/// is renewed. That keeps the fork API-valid and the retry a warm prefix-extension
/// rather than a new request shape.
///
/// A *stream that dies before finalizing* is retried too, by [`peek_completion`], on the
/// same budget the main loop gives its own turns — separate from the compliance budget
/// above, because an aborted stream produced no verdict to be non-compliant with. A
/// failed request (nothing streamed) is still not retried here: the `Client` has
/// already exhausted the transient family for it (docs/transport.md).
///
/// Each attempt's thinking is pushed onto `record.thinking` before its verdict is judged, so a
/// gate that took two attempts records the reasoning of both — the retried attempt's
/// reasoning is what says whether the nudge landed. Text only: the fork's transcript,
/// signatures included, stays throwaway, and this is a record rather than a replay input
/// (docs/redaction-fork.md).
pub(super) async fn peek_with_retries<T>(
    client: &Client,
    mut messages: Vec<WireTurn>,
    tools: &[Value],
    choice: ToolChoice,
    tool: &'static str,
    parse: impl Fn(&Completion) -> Result<T, String>,
    record: &mut PeekRecord,
) -> Result<T, String> {
    for attempt in 1..=PEEK_ATTEMPTS {
        let completion = peek_completion(client, &messages, tools, choice, record).await?;
        record.thinking.extend(peek_thinking(&completion.assistant));
        let parsed = parse(&completion);
        match parsed {
            Ok(verdict) => return Ok(verdict),
            Err(why) => {
                // Durable even when the next attempt recovers: a runtime triage
                // failure is a harness defect signal (docs/redaction-fork.md), so
                // it lands on the gate's `Triage`, not just the daemon's stderr.
                record
                    .retries
                    .push(format!("attempt {attempt} of {PEEK_ATTEMPTS}: {why}"));
                if attempt == PEEK_ATTEMPTS
                    || !extend_with_retry(&mut messages, &completion.assistant, &why, tool)
                {
                    return Err(format!("{why} (attempt {attempt} of {PEEK_ATTEMPTS})"));
                }
                log::warn!("fork peek attempt {attempt}: {why}; retrying in-fork");
            }
        }
    }
    unreachable!("the loop returns on the final attempt")
}

/// One peek round-trip, retried while its **stream dies before finalizing**
/// (docs/transport.md) — the same treatment, on the same budget
/// ([`STREAM_ABORT_ATTEMPTS`]) and the same [`stream_abort_backoff`] schedule that the
/// main loop gives an aborted turn.
///
/// The partial is discarded rather than replayed: whatever arrived is untrusted, and
/// the in-fork compliance retry's trick of replaying the assistant turn does not apply
/// (the API rejects a replayed turn without its thinking signature intact). Nothing
/// needs replaying, because the fork's tail is throwaway — the retry re-sends the
/// byte-identical `messages` over the still-warm prefix, exactly as the main loop's
/// record-only partial arranges for its own replay.
///
/// Why retry at all, when withholding is a defined outcome: withholding hands the
/// agent a call it must decide whether to re-run, and re-running is not free or always
/// safe — a suite re-run against a still-live server whose counters the invisible first
/// run already moved turns a pass into a spurious failure (docs/transport.md). Exhaustion
/// still withholds; the budget is what makes it rare.
///
/// Every attempt's [`RequestUsage`] is pushed — the request happened and slid the
/// prefix's TTL whether or not it finalized — and every abort leaves an
/// attempt-stamped breadcrumb in `record.retries`, so a recovered abort is still visible in
/// the corpus rather than silently absorbed.
pub(super) async fn peek_completion(
    client: &Client,
    messages: &[WireTurn],
    tools: &[Value],
    choice: ToolChoice,
    record: &mut PeekRecord,
) -> Result<Completion, String> {
    let mut aborted: u32 = 0;
    loop {
        // No-op sink: the fork peek's triage tokens are internal and must never surface
        // on the control stream — only the main thread's prose streams.
        let completion = match client.complete(messages, tools, choice, |_| {}).await {
            Ok(completion) => completion,
            Err(e) => {
                client.upstream_retry().clear(RetrySurface::Peek);
                return Err(format!("peek request failed: {e}"));
            }
        };
        record.usage.push(completion.request_usage());
        let Some(why) = completion.upstream_failure().map(str::to_string) else {
            // The attempt that finalizes is the all-clear (docs/transport.md).
            client.upstream_retry().clear(RetrySurface::Peek);
            return Ok(completion);
        };
        aborted += 1;
        if completion.terminal_failure().is_some() {
            // Named by the provider as unretryable (`api/failure.rs`): withhold now
            // rather than repeat the answer up the schedule. The main loop's next
            // request meets the same condition and stalls with it.
            record.retries.push(format!(
                "stream refused upstream (attempt {aborted}): {why}"
            ));
            client.upstream_retry().clear(RetrySurface::Peek);
            return Err(format!("peek refused upstream: {why}"));
        }
        record.retries.push(format!(
            "stream aborted upstream (attempt {aborted} of {STREAM_ABORT_ATTEMPTS}): {why}"
        ));
        let Some(pause) = stream_abort_backoff(aborted) else {
            client.upstream_retry().clear(RetrySurface::Peek);
            return Err(format!(
                "peek stream aborted upstream, {aborted} attempts exhausted: {why}"
            ));
        };
        log::warn!(
            "fork peek stream aborted (attempt {aborted} of \
             {STREAM_ABORT_ATTEMPTS}): {why}; retrying"
        );
        // Published on the same topic the turn loop uses, distinguished by its
        // surface: a peek that exhausts withholds a verdict, where a turn that
        // exhausts ends the trajectory (docs/transport.md).
        client
            .upstream_retry()
            .set(stream_abort_retry(RetrySurface::Peek, aborted, pause, &why));
        tokio::time::sleep(pause).await;
    }
}

/// Extend the fork's throwaway tail past a non-compliant response, keeping the
/// conversation API-valid: the assistant turn is replayed verbatim, every `tool_use` it
/// made is answered with an error `tool_result` carrying `why`, and a renewed ask
/// closes the tail. Returns `false` when nothing appendable came back (no assistant
/// payload, or an empty one) — the caller treats that as terminal rather than sending
/// an invalid message.
pub(super) fn extend_with_retry(
    messages: &mut Vec<WireTurn>,
    assistant: &Payload,
    why: &str,
    tool: &str,
) -> bool {
    let Payload::Assistant { output, .. } = assistant else {
        return false;
    };
    let empty = match output {
        AssistantOutput::Messages(blocks) => blocks.is_empty(),
        AssistantOutput::Responses(items) => items.is_empty(),
    };
    if empty {
        return false;
    }
    messages.push(WireTurn::Assistant(output.clone()));
    let mut reply: Vec<UserPart> = output
        .tool_calls()
        .unwrap_or_default()
        .into_iter()
        .map(|call| UserPart::ToolResult {
            call_id: call.id,
            seq: None,
            content: format!("not accepted — {why}."),
            is_error: true,
            images: Vec::new(),
        })
        .collect();
    reply.push(UserPart::Text(format!(
        "That response didn't answer the ask. Call `{tool}` now, as instructed above."
    )));
    messages.push(WireTurn::User(reply));
    true
}

/// The text of every thinking block one peek attempt emitted, joined — the reasoning
/// behind that attempt's verdict, recorded on the gate's `Triage`
/// (docs/redaction-fork.md). `None` when the attempt thought nothing (or thought only in
/// redacted blocks, which carry no text), so a gate that didn't think records nothing
/// rather than an empty string.
///
/// Only the text: a `signature` is what the API requires to *replay* a thinking block,
/// and nothing replays the fork's tail — it is discarded with the fork.
pub(super) fn peek_thinking(assistant: &Payload) -> Option<String> {
    let Payload::Assistant { output, .. } = assistant else {
        return None;
    };
    let text = output.reasoning_text().join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}
