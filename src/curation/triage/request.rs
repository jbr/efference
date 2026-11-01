//! Round-trips against the model with bounded in-fork retries (docs/triage.md).

use super::*;

/// Fork round-trips per fork: the ask plus bounded in-fork retries
/// (docs/triage.md). Each retry re-reads the warm prefix and appends only to
/// the throwaway tail.
pub(super) const TRIAGE_ATTEMPTS: usize = 3;

/// Forks per triage: when every in-fork retry of one fork comes back unusable, the
/// fork is discarded and a fresh one starts from the top — the ask alone, with none
/// of the rejected turns in its tail (docs/triage.md). A fork that has
/// argued itself into a wrong shape tends to stay there; a fresh one has not.
/// Exhausting every fork is a harness defect, not an upstream condition: the model
/// answered each time, and the ask or the parse is what failed.
pub const TRIAGE_RESTARTS: usize = 3;

/// How long a triage keeps trying (docs/triage.md). The triage waits failures
/// out, because its results reach the agent only through a verdict; the asides —
/// the idle triage and the title — give up early, because each has a harmless
/// fallback and the loop is blocked on it meanwhile.
#[derive(Clone, Copy)]
pub(super) struct Patience {
    /// Fresh forks to start when one fork's in-fork retries are spent.
    forks: usize,
    /// Upstream attempts per request before giving up.
    upstream: u32,
}

impl Patience {
    /// The triage's: every fork, and the main loop's full upstream schedule.
    pub(super) const TRIAGE: Self = Self {
        forks: TRIAGE_RESTARTS,
        upstream: STREAM_ABORT_ATTEMPTS,
    };
    /// An aside's: one fork, and a short upstream schedule.
    pub(super) const ASIDE: Self = Self {
        forks: 1,
        upstream: 3,
    };
}

/// Why a triage produced no verdict (docs/triage.md). Never shown to the agent:
/// the triage holds its results until a verdict arrives, or the operator interrupts.
#[derive(Debug)]
pub enum TriageFailure {
    /// The upstream could not answer — the transient schedule ran out, or it refused
    /// the request outright.
    Upstream(String),
    /// Every fork answered, and never usably: a harness defect.
    Unusable(String),
    /// The operator interrupted while the triage ran.
    Interrupted,
}

impl std::fmt::Display for TriageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Upstream(why) | Self::Unusable(why) => f.write_str(why),
            Self::Interrupted => f.write_str("triage interrupted"),
        }
    }
}

/// What one triage accumulates across its attempts, whatever the verdict turns out to be:
/// the billing record, the compliance breadcrumbs, and the triage's reasoning
/// (docs/triage.md). The three travel together because every attempt appends to
/// all of them and the outcome carries all of them onto the triage's `Triage`.
#[derive(Default)]
pub(super) struct TriageRecord {
    /// One [`RequestUsage`] per request the triage made, in-fork retries and aborted
    /// streams included — the request happened and slid the prefix's cache TTL.
    pub(super) usage: Vec<RequestUsage>,
    /// One attempt-stamped breadcrumb per attempt that had to be retried, recovered or
    /// not.
    pub(super) retries: Vec<String>,
    /// The thinking text of each attempt that thought, in attempt order.
    pub(super) thinking: Vec<String>,
    /// The request the triage keepalive re-sends, where it re-sends this triage's
    /// (docs/transport.md).
    pub(super) sent: Option<SentTriage>,
}

/// The renewal sentence for a triage that asks for `tool` (docs/triage.md).
pub(super) fn call_now(tool: &str) -> String {
    format!("Call `{tool}` now, as instructed above.")
}

/// Run one triage to a usable verdict, with bounded in-fork retries inside bounded
/// fresh forks (docs/triage.md). `renew` is the sentence that re-asks after a
/// non-compliant attempt ([`call_now`] for a tool ask); `parse` decodes a usable verdict out of a completion, or says why it couldn't —
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
/// When a fork's in-fork retries are spent, the fork is dropped and the ask starts
/// again from `messages` ([`TRIAGE_RESTARTS`]); running out of forks is
/// [`TriageFailure::Unusable`].
///
/// An upstream failure — a stream that dies before finalizing, or a request that
/// fails transiently — is retried too, by [`triage_completion`], on the same budget
/// the main loop gives its own turns and separate from the compliance budget
/// above, because it produced no verdict to be non-compliant with. `retry` is the
/// agent's retry cell, where the wait is published and where the operator's
/// "retry now" lands, whichever client the triage sends through.
///
/// Each attempt's thinking is pushed onto `record.thinking` before its verdict is judged, so a
/// triage that took two attempts records the reasoning of both — the retried attempt's
/// reasoning is what says whether the nudge landed. Text only: the fork's transcript,
/// signatures included, stays throwaway, and this is a record rather than a replay input
/// (docs/triage.md).
// Called by the result triage, the idle triage and the title ask, each of which supplies
// every input once and none of which leaves one optional, so an options value would be the
// parameter list under another name.
#[allow(clippy::too_many_arguments)]
pub(super) async fn triage_with_retries<T>(
    client: &Client,
    retry: &UpstreamRetryCell,
    patience: Patience,
    messages: Vec<WireTurn>,
    tools: &[Value],
    choice: ToolChoice,
    renew: &str,
    parse: impl Fn(&Completion) -> Result<T, String>,
    record: &mut TriageRecord,
    on_delta: &(dyn Fn(Streamed) + Sync),
) -> Result<T, TriageFailure> {
    let mut last = String::new();
    let forks = patience.forks;
    for fork in 1..=forks {
        let mut messages = messages.clone();
        for attempt in 1..=TRIAGE_ATTEMPTS {
            let completion = triage_completion(
                client,
                retry,
                patience.upstream,
                &messages,
                tools,
                choice,
                record,
                on_delta,
            )
            .await?;
            record
                .thinking
                .extend(triage_thinking(&completion.assistant));
            let why = match parse(&completion) {
                Ok(verdict) => return Ok(verdict),
                Err(why) => why,
            };
            // Durable even when the next attempt recovers: a runtime triage
            // failure is a harness defect signal (docs/triage.md), so
            // it lands on the triage's `Triage`, not just the daemon's stderr.
            record.retries.push(format!(
                "fork {fork} of {forks}, attempt {attempt} of {TRIAGE_ATTEMPTS}: {why}"
            ));
            last = format!("{why} (fork {fork} of {forks}, attempt {attempt})");
            if attempt == TRIAGE_ATTEMPTS
                || !extend_with_retry(&mut messages, &completion.assistant, &why, renew)
            {
                break;
            }
            log::debug!("triage attempt {attempt}: {why}; retrying in-fork");
        }
        if fork < forks {
            log::debug!("triage fork {fork} produced no usable verdict; starting a fresh fork");
        }
    }
    Err(TriageFailure::Unusable(last))
}

/// One triage round-trip, retried while the **upstream fails transiently**
/// (docs/transport.md) — a stream that dies before finalizing, or a request that
/// could not open one — with the same treatment, on the same budget
/// ([`STREAM_ABORT_ATTEMPTS`], for the triage) and the same [`stream_abort_backoff`]
/// schedule that the main loop gives its own turns. `budget` is the attempt count.
///
/// The partial is discarded rather than replayed: whatever arrived is untrusted, and
/// the in-fork compliance retry's trick of replaying the assistant turn does not apply
/// (the API rejects a replayed turn without its thinking signature intact). Nothing
/// needs replaying, because the fork's tail is throwaway — the retry re-sends the
/// byte-identical `messages` over the still-warm prefix, exactly as the main loop's
/// record-only partial arranges for its own replay.
///
/// Every attempt that got a response pushes its [`RequestUsage`] — the request
/// happened and slid the prefix's TTL whether or not it finalized — and every failure
/// leaves an attempt-stamped breadcrumb in `record.retries`, so a recovered failure is
/// still visible in the corpus rather than silently absorbed. The wait is published on
/// `retry` and ends early on the operator's "retry now".
// Called only by `triage_with_retries`, which holds every input except the attempt's
// messages fixed across its loop and passes them through.
#[allow(clippy::too_many_arguments)]
pub(super) async fn triage_completion(
    client: &Client,
    retry: &UpstreamRetryCell,
    budget: u32,
    messages: &[WireTurn],
    tools: &[Value],
    choice: ToolChoice,
    record: &mut TriageRecord,
    on_delta: &(dyn Fn(Streamed) + Sync),
) -> Result<Completion, TriageFailure> {
    let mut failed: u32 = 0;
    loop {
        // The fragments go to `on_delta`, which publishes them on their own `output`
        // kinds (docs/control-api.md) — never as the main loop's response.
        let (why, terminal) = match client.complete(messages, tools, choice, on_delta).await {
            Ok(completion) => {
                record.usage.push(completion.request_usage());
                let Some(why) = completion.upstream_failure().map(str::to_string) else {
                    // The attempt that finalizes is the all-clear (docs/transport.md).
                    retry.clear(RetrySurface::Triage);
                    return Ok(completion);
                };
                if completion.operator_abort() {
                    // The operator interrupted during the triage (docs/control-channel.md):
                    // the latch the press set is left for the loop, which consumes it
                    // after the triage and ends the turn.
                    record.retries.push(format!("triage ended: {why}"));
                    retry.clear(RetrySurface::Triage);
                    return Err(TriageFailure::Interrupted);
                }
                (why, completion.terminal_failure().is_some())
            }
            Err(e) => {
                let terminal = crate::inference::api::error_class(&e)
                    != crate::inference::api::FailureClass::Transient;
                (format!("triage request failed: {e}"), terminal)
            }
        };
        failed += 1;
        if terminal {
            // `error_class` names it unretryable: stop now rather
            // than repeat the answer up the schedule.
            record
                .retries
                .push(format!("triage refused upstream (attempt {failed}): {why}"));
            retry.clear(RetrySurface::Triage);
            return Err(TriageFailure::Upstream(format!(
                "triage refused upstream: {why}"
            )));
        }
        record.retries.push(format!(
            "triage failed upstream (attempt {failed} of {budget}): {why}"
        ));
        let Some(pause) = stream_abort_backoff(failed).filter(|_| failed < budget) else {
            retry.clear(RetrySurface::Triage);
            return Err(TriageFailure::Upstream(format!(
                "triage failed upstream, {failed} attempts exhausted: {why}"
            )));
        };
        log::debug!("triage attempt {failed} of {budget} failed upstream: {why}; retrying");
        // Published on the same topic the turn loop uses, distinguished by its
        // surface (docs/transport.md).
        retry.set(stream_abort_retry(
            RetrySurface::Triage,
            failed,
            budget,
            pause,
            &why,
        ));
        // The loop's own wait, published as its `delayed` phase; a title ask is a
        // detached aside, so its wait is not the loop's (docs/control-api.md).
        if client.origin() == RequestOrigin::Title {
            retry.pause(Some(pause)).await;
        } else {
            retry.delay(pause).await;
        }
    }
}

/// Extend the fork's throwaway tail past a non-compliant response, keeping the
/// conversation API-valid: the assistant turn is replayed verbatim, every `tool_use` it
/// made is answered with an error `tool_result` carrying `why`, and `renew` closes
/// the tail as the re-ask. Returns `false` when nothing appendable came back (no assistant
/// payload, or an empty one) — the caller treats that as terminal rather than sending
/// an invalid message.
pub(super) fn extend_with_retry(
    messages: &mut Vec<WireTurn>,
    assistant: &Payload,
    why: &str,
    renew: &str,
) -> bool {
    let Payload::Assistant { output, .. } = assistant else {
        return false;
    };
    let empty = match output {
        AssistantOutput::Messages(blocks) => blocks.is_empty(),
        AssistantOutput::Responses(items) => items.is_empty(),
        AssistantOutput::Completions(message) => message.is_empty(),
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
        "That response didn't answer the ask. {renew}"
    )));
    messages.push(WireTurn::User(reply));
    true
}

/// The text of every thinking block one triage attempt emitted, joined — the reasoning
/// behind that attempt's verdict, recorded on the triage's `Triage`
/// (docs/triage.md). `None` when the attempt thought nothing (or thought only in
/// redacted blocks, which carry no text), so a triage that didn't think records nothing
/// rather than an empty string.
///
/// Only the text: a `signature` is what the API requires to *replay* a thinking block,
/// and nothing replays the fork's tail — it is discarded with the fork.
pub(super) fn triage_thinking(assistant: &Payload) -> Option<String> {
    let Payload::Assistant { output, .. } = assistant else {
        return None;
    };
    let text = output.reasoning_text().join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}
