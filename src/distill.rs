//! Distillation — what a raw tool result projects to on the canonical wire
//! (docs/redaction-fork.md, docs/README.md), and the inbox triage that rides the
//! same fork peek (docs/inbox.md).
//!
//! Every tool result is distilled at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The distiller's job is to produce a **verdict** — which
//! lines survive (`keep`) plus a `note` — not rendered bytes: the wire form is
//! re-rendered from `raw` + the verdict at projection time
//! ([`crate::project::render_distilled`], docs/redaction-fork.md), so the kept bytes
//! are never stored twice and the structured decision doubles as the audit record.
//!
//! The same throwaway peek also triages the **inbox** (docs/inbox.md): the pending
//! ambient notifications are shown under the output, and the `retain` verdict's
//! `inbox` field pins / dismisses them by `[seq]`. The dispositions ride back on the
//! [`Distillation`] and land on the logged [`Triage`](crate::log::Triage). One type
//! does the distilling:
//!
//! - [`ForkPeek`] — the universal triage gate (docs/redaction-fork.md): fork a
//!   throwaway branch off the cached prefix, show the model the line-numbered raw
//!   output + the inbox, and ask it (via the tail instruction, never a forced
//!   `tool_choice`) to call `retain` to choose what the canonical wire keeps and which
//!   notifications to pin. The peek sends the *same* request envelope as an agent turn —
//!   one shared `tools[]`, `tool_choice: auto`, the same thinking policy — so both
//!   shapes read one cache lineage (docs/redaction-fork.md, docs/log-and-projection.md).
//!   A non-compliant response gets a bounded in-fork retry; exhaustion is a **hard
//!   error** — the results project [`WireState::TriageFailed`] (an error in place of
//!   the output; the raw stays in the log) and the inbox is left untouched. The full
//!   untriaged raw is never welded onto the canonical wire.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{STREAM_ABORT_ATTEMPTS, stream_abort_backoff, stream_abort_retry};
use crate::api::{Client, Completion, RequestUsage, RetrySurface, ToolChoice};
use crate::context::Item;
use crate::inbox::ShownNotification;
use crate::log::{
    Disposition, Keep, LogEntry, Origin, Payload, RawResult, Seq, ToolCall, WireState,
};
use crate::project::{
    ApiBlock, ApiMessage, ImagePlacement, PEEK_LINE_CAP, Role, block_to_api,
    place_trailing_cache_breakpoint, project_with,
};
use crate::supervisor::BgView;

const RETAIN: &str = "retain";

/// The tool of the **standalone** idle-triage peek (docs/supervisor.md): no `keep`
/// (there's no tool output to retain), only inbox/background dispositions.
const TRIAGE: &str = "triage";

/// Whether `name` is a peek tool. Both stand in every turn's `tools[]` (the shared
/// envelope, docs/redaction-fork.md) but are answered only when a peek's tail
/// instruction asks — the agent loop pairs an un-asked call with a nudge result
/// instead of running the cascade.
pub fn is_peek_tool(name: &str) -> bool {
    name == RETAIN || name == TRIAGE
}

/// Fork round-trips per gate: the ask plus bounded in-fork retries
/// (docs/redaction-fork.md). Each retry re-reads the warm prefix and appends only to
/// the throwaway tail; exhaustion withholds the output as a hard error, which the
/// agent may answer by re-issuing the call — so retrying here is cheaper than making
/// the agent recover.
const PEEK_ATTEMPTS: usize = 3;

/// What one gate accumulates across its attempts, whatever the verdict turns out to be:
/// the billing record, the compliance breadcrumbs, and the peek's reasoning
/// (docs/redaction-fork.md). The three travel together because every attempt appends to
/// all of them and the outcome carries all of them onto the gate's `Triage`.
#[derive(Default)]
struct PeekRecord {
    /// One [`RequestUsage`] per request the gate made, in-fork retries and aborted
    /// streams included — the request happened and slid the prefix's cache TTL.
    usage: Vec<RequestUsage>,
    /// One attempt-stamped breadcrumb per attempt that had to be retried, recovered or
    /// not.
    retries: Vec<String>,
    /// The thinking text of each attempt that thought, in attempt order.
    thinking: Vec<String>,
}

/// A live-process triage disposition (docs/supervisor.md) — distinct from the seq-keyed
/// notification [`Disposition`] because a running process is addressed by its live
/// `p#<n>` handle, not a frozen arrival seq, and its "dismiss" is **mute** (stop
/// surfacing the tail; the process keeps running), not clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgVerdict {
    /// Snapshot the current tail onto the canonical wire — the loop materializes it
    /// as a pinned `Ambient` at the `Triage` position (reusing the notification pin
    /// path).
    Pin,
    /// Stop surfacing the running tail; the process runs on and its exit still notifies.
    Mute,
}

/// One tool result awaiting triage: the call that produced it and its raw output.
/// A gate covers every result a single completion produced (docs/redaction-fork.md),
/// so the loop hands the distiller a slice of these rather than one at a time.
pub struct Pending<'a> {
    pub call: &'a ToolCall,
    pub raw: &'a RawResult,
}

/// The known-shape rule (docs/redaction-fork.md): the name of the verbatim handle
/// this result carries, if any. A handle-bearing result is **bypassed** — kept out
/// of the ask, decided by the harness, and delivered raw — because the peek has
/// nothing in front of it that says the text is unsummarizable.
fn bypassed(pending: &Pending<'_>) -> Option<&'static str> {
    verbatim_handle(&pending.raw.content)
}

/// The call ids this gate asks about: every pending result the shape rule did not
/// bypass. This is the verdict's coordinate system, so the bypassed ids appear in
/// neither the ask nor the coverage check — the peek is never asked to decide
/// something the harness already decided.
fn offered_ids(pending: &[Pending<'_>]) -> Vec<String> {
    pending
        .iter()
        .filter(|p| bypassed(p).is_none())
        .map(|p| p.call.id.0.clone())
        .collect()
}

/// The harness's verdict on a bypassed result (docs/redaction-fork.md):
/// [`WireState::Raw`], so the whole body — handle included — projects verbatim.
/// `origin` is `Fallback` because no model produced it; `raw_keep` names the shape,
/// which is what separates a bypass from a failed gate in the log after the fact.
fn bypass_verdict(shape: &str) -> Distillation {
    Distillation {
        wire: WireState::Raw,
        origin: Origin::Fallback,
        raw_keep: format!("bypassed triage: {shape} — delivered verbatim"),
    }
}

/// The whole gate's outcome when the shape rule bypassed **every** result: decided
/// without a request at all, since there is nothing left to ask about — a skipped
/// verdict is also a skipped round-trip. `None` means at least one result is still
/// the fork's, and the peek runs.
///
/// The gate-scoped riders (inbox, background, surfacing) simply wait for the next
/// gate: an undispositioned notification stays active and is re-shown there
/// (docs/inbox.md), so nothing is lost by not opening a peek for them here.
fn bypassed_gate(pending: &[Pending<'_>]) -> Option<GateOutcome> {
    let results: Vec<Distillation> = pending
        .iter()
        .map_while(|p| bypassed(p).map(bypass_verdict))
        .collect();
    (results.len() == pending.len()).then(|| GateOutcome {
        results,
        ..GateOutcome::default()
    })
}

/// The verdict on **one** tool result (docs/redaction-fork.md): the wire form plus the
/// audit pieces the loop stamps into that result's [`Triage`](crate::log::Triage)
/// entry. `raw` is never mutated — it stays in the log.
pub struct Distillation {
    /// The canonical wire form — a [`WireState::Distilled`] verdict (kept ranges +
    /// note), re-rendered from `raw` at projection time (docs/redaction-fork.md).
    pub wire: WireState,
    /// Whether the verdict is the model's or a harness fallback (docs/redaction-fork.md).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse — empty for a model-free distiller.
    pub raw_keep: String,
}

/// The outcome of one triage gate (docs/redaction-fork.md). A gate rides the results of
/// a single completion — usually one, but a provider that ignores the single-call
/// request (docs/README.md) returns several, and all of them are triaged together in
/// one peek. That grouping is what keeps the peek's request valid: every `tool_use` in
/// the trailing assistant message is answered by a real result in the same tail, so
/// nothing is ever orphaned.
///
/// The inbox / background / surfacing riders are **gate-scoped**, not per-result: they
/// are decided once for the step, which is why they sit here rather than on
/// [`Distillation`]. `Default` is the outcome of a gate with nothing to triage.
#[derive(Default)]
pub struct GateOutcome {
    /// One verdict per [`Pending`] handed in, in the same order. Always the same
    /// length as the input — a failed peek yields [`WireState::TriageFailed`]
    /// verdicts, never a short vector, so the loop can zip them without checking.
    pub results: Vec<Distillation>,
    /// Per-notification dispositions this peek chose, by arrival `seq` (docs/inbox.md) —
    /// filtered to notifications actually shown. Empty when nothing was pinned or
    /// dismissed (everything left for the next gate).
    pub inbox: Vec<(Seq, Disposition)>,
    /// Live-process dispositions this peek chose, by `p#<n>` handle (docs/supervisor.md) —
    /// filtered to processes actually shown. The loop materializes a `Pin` into a
    /// snapshot `Ambient` and applies a `Mute` to the supervisor.
    pub background: Vec<(String, BgVerdict)>,
    /// Context-item names this peek chose to surface (docs/redaction-fork.md) —
    /// filtered to the roster actually shown (invented names dropped). Stored on the
    /// `Triage` (`surfaced`); `project` re-renders each as a read-me reminder. Empty
    /// when nothing was surfaced.
    pub surfaced: Vec<String>,
    /// The [`RequestUsage`] of every request this gate made — empty for a model-free
    /// distiller, one entry per fork round-trip (in-fork retries included). Each fork
    /// request reads the shared canonical prefix (sliding its cache TTL), and the loop
    /// folds them all into the cache telemetry (docs/log-and-projection.md).
    pub usage: Vec<RequestUsage>,
    /// One attempt-stamped breadcrumb per peek attempt that had to be retried —
    /// recorded even when a retry recovered (docs/redaction-fork.md). Two families,
    /// distinguishable by wording: a non-compliant verdict (wrong tool, no tool,
    /// malformed input), and a stream that died before finalizing. The loop lands
    /// these on the gate's `Triage` (`retries`), the durable record; a runtime triage
    /// failure is never accepted silently, recovered or not.
    pub retries: Vec<String>,
    /// The reasoning behind the verdict — one entry per peek attempt that emitted
    /// thinking, in attempt order (docs/redaction-fork.md). The loop lands these on the
    /// gate's `Triage` (`thinking`), which is the only record of why a verdict came out
    /// the way it did; the fork's transcript is still discarded.
    pub thinking: Vec<String>,
}

impl GateOutcome {
    /// The hard-error outcome for a whole gate (docs/redaction-fork.md): the peek
    /// could not elicit a usable verdict (a transport failure, or in-fork retry
    /// exhaustion), so every result projects [`WireState::TriageFailed`] — an error
    /// in place of the output, `is_error` forced true, exactly as if the call itself
    /// had failed. The full untriaged raw is **never** the fallback: `WireState` is
    /// fixed once, so an uncurated result would stay in the cached prefix for the
    /// rest of the session with every later turn paying for it. `raw` stays in the
    /// log; the agent decides whether to re-issue the call, and the error is visible
    /// to the operator in the transcript like any failed call.
    ///
    /// `why` records the cause — it renders in each error result and lands in each
    /// result's `raw_keep`, the field the `Triage` entry carries for audit. Without
    /// it a failure is anonymous: a malformed verdict, a wrong-tool call, and a
    /// transport error all read identically after the fact.
    pub fn failed(count: usize, usage: Vec<RequestUsage>, why: &str) -> Self {
        Self {
            results: (0..count)
                .map(|_| Distillation {
                    wire: WireState::TriageFailed {
                        why: why.to_string(),
                    },
                    origin: Origin::Fallback,
                    raw_keep: why.to_string(),
                })
                .collect(),
            inbox: Vec::new(),
            background: Vec::new(),
            surfaced: Vec::new(),
            usage,
            retries: Vec::new(),
            thinking: Vec::new(),
        }
    }
}

/// The verdict of a **standalone** idle-triage peek (docs/supervisor.md) — a gate that rides no
/// tool result, so it has no [`WireState`]; just the inbox/background dispositions (and
/// the audit pieces the loop stamps into a [`Triage`](crate::log::Triage) with
/// `for_result: None`).
pub struct InboxTriage {
    pub inbox: Vec<(Seq, Disposition)>,
    pub background: Vec<(String, BgVerdict)>,
    pub origin: Origin,
    /// One entry per request the peek made (in-fork retries included), like
    /// [`GateOutcome::usage`]; empty for a model-free distiller or a transport failure.
    pub usage: Vec<RequestUsage>,
    /// Non-compliant-attempt breadcrumbs, like [`GateOutcome::retries`].
    pub retries: Vec<String>,
    /// Per-attempt peek reasoning, like [`GateOutcome::thinking`].
    pub thinking: Vec<String>,
}

/// The model peek — the universal triage gate (docs/redaction-fork.md) realized.
///
/// The gate is a concrete type, not a trait object. It was built behind a
/// single-method `Distiller` trait threaded as a generic bound through
/// `agent::run`, `daemon::serve`, and `run_idle_triage`; the second implementation
/// that bound existed for never arrived, and the one that briefly did (a model-free
/// `HeadTail` stub) had no call sites, tests included. Swapping the *model* behind
/// the peek — the measured experiment — is a field on this struct and a different
/// `Client`, not another implementation; a materially different peek *shape* (a
/// compacted second projection, a different transcript budget) is what would earn
/// the indirection back.
pub struct ForkPeek {
    /// The session's provider-derived image placement — the fork's `project` of
    /// the canonical prefix must be byte-identical to the agent turn's, or the
    /// peek pays a cold prefix instead of a cache read (docs/redaction-fork.md,
    /// docs/log-and-projection.md).
    pub image_placement: ImagePlacement,
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
async fn peek_with_retries<T>(
    client: &Client,
    mut messages: Vec<ApiMessage>,
    tools: &[Value],
    tool: &str,
    parse: impl Fn(&Completion) -> Result<T, String>,
    record: &mut PeekRecord,
) -> Result<T, String> {
    for attempt in 1..=PEEK_ATTEMPTS {
        let completion = peek_completion(client, &messages, tools, record).await?;
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
/// safe. In a controlled run a withheld conformance `PASS` was re-earned by re-running
/// the suite against a still-live server, whose counters the invisible first run had
/// already moved — turning a pass into a spurious failure and costing nine turns
/// (docs/transport.md). Exhaustion still withholds; it now takes several attempts to
/// reach.
///
/// Every attempt's [`RequestUsage`] is pushed — the request happened and slid the
/// prefix's TTL whether or not it finalized — and every abort leaves an
/// attempt-stamped breadcrumb in `record.retries`, so a recovered abort is still visible in
/// the corpus rather than silently absorbed.
async fn peek_completion(
    client: &Client,
    messages: &[ApiMessage],
    tools: &[Value],
    record: &mut PeekRecord,
) -> Result<Completion, String> {
    let mut aborted: u32 = 0;
    loop {
        // No-op sink: the fork peek's triage tokens are internal and must never surface
        // on the control stream — only the main thread's prose streams.
        let completion = match client
            .complete(messages, tools, ToolChoice::auto(), |_| {})
            .await
        {
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
fn extend_with_retry(
    messages: &mut Vec<ApiMessage>,
    assistant: &Payload,
    why: &str,
    tool: &str,
) -> bool {
    let Payload::Assistant {
        thinking,
        text,
        calls,
        ..
    } = assistant
    else {
        return false;
    };
    let mut content: Vec<ApiBlock> = thinking
        .iter()
        .chain(text.iter())
        .map(block_to_api)
        .collect();
    for call in calls {
        content.push(ApiBlock::ToolUse {
            id: call.id.0.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            cache_control: None,
        });
    }
    if content.is_empty() {
        return false;
    }
    messages.push(ApiMessage {
        role: Role::Assistant,
        content,
    });
    let mut reply: Vec<ApiBlock> = calls
        .iter()
        .map(|call| ApiBlock::ToolResult {
            tool_use_id: call.id.0.clone(),
            content: format!("not accepted — {why}.").into(),
            is_error: true,
            cache_control: None,
        })
        .collect();
    reply.push(ApiBlock::Text {
        text: format!(
            "That response didn't answer the ask. Call `{tool}` now, as instructed above."
        ),
        cache_control: None,
    });
    messages.push(ApiMessage {
        role: Role::User,
        content: reply,
    });
    true
}

/// Verdict validity includes **coverage** (docs/redaction-fork.md): every offered call
/// id must be answered, or the verdict is rejected and retried in-fork — a result
/// silently left untriaged is exactly the fallback this gate no longer has.
///
/// The rejection names what the verdict *did* answer, not only what it missed. Without
/// that, "answered nothing" and "answered twenty-six results that were never offered"
/// are the same line in the log, and the second is a prompt-scoping defect the first
/// gives no hint of — which is how a session's worth of gates failed this way before
/// anyone could read the cause off the record (docs/redaction-fork.md on the breadcrumb).
fn require_coverage(verdict: ParsedRetain, offered: &[String]) -> Result<ParsedRetain, String> {
    let unanswered: Vec<&str> = offered
        .iter()
        .filter(|id| !verdict.results.iter().any(|v| &v.call_id == *id))
        .map(String::as_str)
        .collect();
    if unanswered.is_empty() {
        return Ok(verdict);
    }
    let answered = match verdict.results.as_slice() {
        [] => "nothing".to_string(),
        answered => format!(
            "[{}]",
            answered
                .iter()
                .map(|v| v.call_id.as_str())
                .collect::<Vec<_>>()
                .join("], [")
        ),
    };
    Err(format!(
        "verdict answered no result for [{}] — it answered {answered}",
        unanswered.join("], [")
    ))
}

/// The number of numbered lines the peek view shows for a raw result — the bound a
/// `keep` range must land inside. Counts the way [`result_view`] numbers and
/// `distillate` caps (docs/redaction-fork.md): all three must agree or the verdict's
/// coordinates drift from what the model saw.
fn shown_line_count(raw: &RawResult) -> usize {
    raw.content.lines().count().min(PEEK_LINE_CAP)
}

/// Verdict validity also includes **bounds** (docs/redaction-fork.md): a `keep` range
/// lying wholly outside the shown line numbers is a coordinate mistake, not a decision —
/// observed live when a peek adopted the file line numbers embedded in `grep -n` output
/// as its retain coordinates — and render-time normalization would drop it, turning
/// "keep these lines" into silent total loss with no signal on either side. Rejected
/// here instead, the mistake becomes an in-fork retry whose nudge names the actual
/// bound. One out-of-range range fails the whole verdict: a partial drop is the same
/// silent loss at smaller scale, and the retry is a warm prefix extension.
///
/// Overshoot from an in-bounds start stays acceptable (render clamps the tail — the
/// peek's line accounting routinely runs a line or two past the rendering's). A 0 start
/// is rejected rather than clamped: `0-5` under zero-indexing means shown lines 1–6,
/// as a typo'd lower bound it means 1–5, and guessing either silently keeps the wrong
/// lines.
///
/// `shown` maps each offered call id to its shown line count; a verdict keyed to an id
/// that was never offered has no bound to check and passes through — the caller's
/// reconciliation drops it, the same guard as everywhere else.
fn require_in_bounds(
    verdict: ParsedRetain,
    shown: &[(String, usize)],
) -> Result<ParsedRetain, String> {
    for v in &verdict.results {
        let Keep::Ranges(ranges) = &v.keep else {
            continue;
        };
        let Some(&(_, n)) = shown.iter().find(|(id, _)| *id == v.call_id) else {
            continue;
        };
        for &(s, e) in ranges {
            let range = if s == e {
                format!("line {s}")
            } else {
                format!("range {s}-{e}")
            };
            if s.min(e) == 0 {
                return Err(format!(
                    "keep {range} for [{}] starts at 0 — the shown line numbers start at 1",
                    v.call_id
                ));
            }
            if s.min(e) > n {
                return Err(format!(
                    "keep {range} for [{}] is beyond the shown output, which has {n} \
                     lines — keep ranges index the line numbers shown, not numbers \
                     inside the output",
                    v.call_id
                ));
            }
        }
    }
    Ok(verdict)
}

/// The text of every thinking block one peek attempt emitted, joined — the reasoning
/// behind that attempt's verdict, recorded on the gate's `Triage`
/// (docs/redaction-fork.md). `None` when the attempt thought nothing (or thought only in
/// redacted blocks, which carry no text), so a gate that didn't think records nothing
/// rather than an empty string.
///
/// Only the text: a `signature` is what the API requires to *replay* a thinking block,
/// and nothing replays the fork's tail — it is discarded with the fork.
fn peek_thinking(assistant: &Payload) -> Option<String> {
    let Payload::Assistant { thinking, .. } = assistant else {
        return None;
    };
    let text = thinking
        .iter()
        .filter_map(|b| match b {
            crate::log::ContentBlock::Thinking { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

/// Describe a completion that produced no usable verdict, for the `Triage` breadcrumb.
/// The model's actual input is the most useful form when the asked-for tool was called —
/// that is what a future reader needs to tell a malformed verdict from a wrong-tool call.
fn unusable(completion: &Completion, tool: &str) -> String {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return "peek returned no assistant payload".into();
    };
    match calls.iter().find(|c| c.name == tool) {
        Some(c) => format!("unparseable {tool} input: {}", c.input),
        None if calls.is_empty() => "peek called no tool".into(),
        None => format!(
            "peek called {} instead of {tool}",
            calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Build the fork peek's request (docs/redaction-fork.md): the cached
/// prefix-through-`tool_use`, then one user message carrying every result plus the
/// gate-scoped riders and the retain prompt.
///
/// **The invariant this exists to hold:** the trailing assistant message carries one
/// `tool_use` per call the completion made, and the API rejects a request that leaves
/// any of them unanswered. So the tail emits one `tool_result` per [`Pending`] — which
/// is only correct because the loop runs *every* call of a completion before opening the
/// gate. Peeking between calls would leave the later ones orphaned, which is exactly the
/// failure this replaced.
///
/// The riders (inbox, background, surfacing roster) are gate-scoped, so they ride the
/// trailing text block rather than any one result. Pure and separate from the round-trip
/// so the pairing invariant is testable without a live client.
fn peek_messages(
    prefix: &[LogEntry],
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
    placement: ImagePlacement,
) -> Vec<ApiMessage> {
    let mut sections = String::new();
    append_inbox_and_background(&mut sections, inbox, background);
    append_roster(&mut sections, roster);

    // Scoped to exactly this gate's ids, and shape-selected on its size — the tail is
    // discarded with the fork, so nothing here is cache-keyed (docs/redaction-fork.md).
    // A rider's field row is offered only when its section is actually above.
    let offered = offered_ids(pending);
    let mut riders: Vec<&str> = Vec::new();
    if !inbox.is_empty() {
        riders.push(INBOX_FIELD);
    }
    if !background.is_empty() {
        riders.push(BACKGROUND_FIELD);
    }
    if roster_shown(roster) {
        riders.push(SURFACE_FIELD);
    }
    let instruction = retain_instruction(&offered, &riders);

    // The prefix hits cache; only the raws and the instruction are fresh, and they are
    // discarded — the canonical lineage never sees raw (docs/redaction-fork.md).
    let mut messages = project_with(prefix, placement);
    place_trailing_cache_breakpoint(&mut messages);
    // Every call is answered — a `tool_use` left unpaired makes the request invalid —
    // but a bypassed result answers with a stand-in rather than its output: the harness
    // already decided it (docs/redaction-fork.md), and the ask above does not offer it.
    let mut content: Vec<ApiBlock> = pending
        .iter()
        .map(|p| ApiBlock::ToolResult {
            tool_use_id: p.call.id.0.clone(),
            content: match bypassed(p) {
                Some(shape) => bypassed_view(p.call, shape),
                None => result_view(p.call, p.raw),
            }
            .into(),
            is_error: p.raw.is_error,
            cache_control: None,
        })
        .collect();
    // `sections` already opens with its own blank line when non-empty, and is empty
    // outright on a gate carrying no riders — so trim rather than always joining with a
    // gap, which left the block opening on stray whitespace.
    content.push(ApiBlock::Text {
        text: match sections.trim() {
            "" => instruction,
            sections => format!("{sections}\n\n{instruction}"),
        },
        cache_control: None,
    });
    messages.push(ApiMessage {
        role: Role::User,
        content,
    });
    messages
}

/// What one un-retried fork peek came back with — the `peek` dev tool's measurement
/// primitive (docs/redaction-fork.md).
///
/// The gate's own machinery retries, so a session's `Triage.retries` records *that* a
/// first attempt failed but never lets you compare two asks: a change that halves
/// first-attempt non-compliance and one that changes nothing both end in a usable
/// verdict. This runs exactly one round trip and reports the first attempt's verdict,
/// which is the number an instruction change actually moves.
pub struct PeekAttempt {
    /// The tools the peek called, in order — empty for a prose-only answer.
    pub called: Vec<String>,
    /// Each call's arguments, alongside `called`. A tool name says the peek reached for
    /// the wrong thing; the arguments say *what it was reaching for* — whether it was
    /// continuing the task, re-reading what it had just been shown, or something else
    /// entirely. That distinction is the whole diagnosis and the name alone erases it.
    pub inputs: Vec<Value>,
    /// The prose and thinking the peek emitted, joined. A non-compliant attempt that
    /// reasoned about triage first is a different failure from one that never registered
    /// the ask, and only the text separates them.
    pub said: String,
    /// Why the verdict was unusable, in the same phrasing `Triage.retries` records.
    /// `None` means a usable, covering verdict on the first attempt.
    pub why: Option<String>,
    /// The model's exact `keep` string when it did answer, for eyeballing quality
    /// separately from compliance.
    pub raw_keep: Option<String>,
    /// The one-line `note`, which is where an invented claim would show up.
    pub note: Option<String>,
    pub usage: RequestUsage,
}

impl PeekAttempt {
    /// A usable, covering verdict on the first try — the thing being measured.
    pub fn compliant(&self) -> bool {
        self.why.is_none()
    }
}

/// Rebuild the exact request a recorded gate's peek sent — the same `peek_messages` the
/// live gate calls, so a replay can't drift from production (backlog/peek-dev-tools.md).
/// Split out for the `peek` dev tool's dry run, which reviews an ask without spending a
/// request on it.
pub fn peek_request(
    prefix: &[LogEntry],
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    placement: ImagePlacement,
) -> Vec<ApiMessage> {
    let roster = surfaceable_roster(prefix);
    peek_messages(prefix, pending, inbox, background, &roster, placement)
}

/// One fork-peek round trip against a recorded gate, with **no retries**
/// (backlog/peek-dev-tools.md). Same request, same validity rule (`parse_retain` plus
/// [`require_coverage`] and [`require_in_bounds`]) as the live gate — only the retry
/// loop is absent, because the first attempt is the measurement.
pub async fn peek_attempt(
    client: &Client,
    tools: &[Value],
    prefix: &[LogEntry],
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
) -> Result<PeekAttempt, String> {
    let offered = offered_ids(pending);
    let shown_lines: Vec<(String, usize)> = pending
        .iter()
        .filter(|p| bypassed(p).is_none())
        .map(|p| (p.call.id.0.clone(), shown_line_count(p.raw)))
        .collect();
    let messages = peek_request(prefix, pending, inbox, background, client.image_placement());
    let completion = client
        .complete(&messages, tools, ToolChoice::auto(), |_| {})
        .await
        .map_err(|e| format!("peek request failed: {e}"))?;
    if let Some(why) = completion.upstream_failure() {
        return Err(format!("peek stream aborted upstream: {why}"));
    }

    let (called, inputs, said) = match &completion.assistant {
        Payload::Assistant {
            calls,
            text,
            thinking,
            ..
        } => (
            calls.iter().map(|c| c.name.clone()).collect(),
            calls.iter().map(|c| c.input.clone()).collect(),
            thinking
                .iter()
                .chain(text)
                .map(|b| match b {
                    crate::log::ContentBlock::Text(t) => t.as_str(),
                    crate::log::ContentBlock::Thinking { text, .. } => text.as_str(),
                    _ => "",
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string(),
        ),
        _ => (Vec::new(), Vec::new(), String::new()),
    };
    let verdict = parse_retain(&completion, &offered)
        .ok_or_else(|| unusable(&completion, RETAIN))
        .and_then(|v| require_coverage(v, &offered))
        .and_then(|v| require_in_bounds(v, &shown_lines));
    let (why, raw_keep, note) = match verdict {
        Ok(v) => {
            let first = v.results.first();
            (
                None,
                first.map(|r| r.raw_keep.clone()),
                first.and_then(|r| r.note.clone()),
            )
        }
        Err(why) => (Some(why), None, None),
    };
    Ok(PeekAttempt {
        called,
        inputs,
        said,
        why,
        raw_keep,
        note,
        usage: completion.request_usage(),
    })
}

impl ForkPeek {
    /// Decide the wire form of the results a completion produced, and triage the inbox +
    /// live background processes (docs/redaction-fork.md, docs/inbox.md, docs/supervisor.md).
    /// `tools` is the turn's full `tools[]` — the fork sends the *same* array as the agent
    /// turn so both shapes share one cache lineage (docs/redaction-fork.md); `prefix` is the
    /// log through the assistant turn that made the calls (the fork's cached head);
    /// `pending` is every result that turn produced, in call order; `inbox` is the pending
    /// notifications and `background` the running processes' fresh tails to show.
    ///
    /// Taking `pending` as a slice rather than one result is what makes the peek valid for a
    /// multi-call completion: the trailing assistant message carries N `tool_use` blocks, so
    /// the peek's tail must answer all N (docs/redaction-fork.md). One gate, N verdicts.
    pub async fn distill(
        &self,
        client: &Client,
        tools: &[Value],
        prefix: &[LogEntry],
        pending: &[Pending<'_>],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> GateOutcome {
        if pending.is_empty() {
            return GateOutcome::default();
        }
        // The ids this gate is asking about, in call order — the `retain` verdict's
        // coordinate system, minus the results the shape rule bypassed. Coverage of
        // every offered id is part of verdict *validity* ([`require_coverage`]): an
        // incomplete verdict retries in-fork rather than leaving a result untriaged.
        if let Some(outcome) = bypassed_gate(pending) {
            return outcome;
        }
        let offered = offered_ids(pending);
        let shown_lines: Vec<(String, usize)> = pending
            .iter()
            .filter(|p| bypassed(p).is_none())
            .map(|p| (p.call.id.0.clone(), shown_line_count(p.raw)))
            .collect();

        let roster = surfaceable_roster(prefix);
        let messages = peek_messages(
            prefix,
            pending,
            inbox,
            background,
            &roster,
            self.image_placement,
        );

        let mut record = PeekRecord::default();
        let parsed = peek_with_retries(
            client,
            messages,
            tools,
            RETAIN,
            |c| {
                let verdict = parse_retain(c, &offered).ok_or_else(|| unusable(c, RETAIN))?;
                let verdict = require_coverage(verdict, &offered)?;
                require_in_bounds(verdict, &shown_lines)
            },
            &mut record,
        )
        .await;

        let verdict = match parsed {
            Ok(v) => v,
            Err(why) => {
                // No usable verdict. The results are withheld as hard errors
                // (docs/redaction-fork.md) — never kept untriaged, which would weld
                // the full raw into the cached prefix for the rest of the session.
                // The error is on the wire, so the agent can re-issue the call and
                // the operator sees the failure in the transcript; the raw stays in
                // the log, and the breadcrumb classifies what came back.
                log::error!(
                    "fork peek produced no usable verdict for {} result(s); withholding \
                     output as a hard error — {why}",
                    offered.len()
                );
                let mut outcome = GateOutcome::failed(pending.len(), record.usage, &why);
                outcome.retries = record.retries;
                // A gate that failed is the one whose reasoning most needs reading.
                outcome.thinking = record.thinking;
                return outcome;
            }
        };

        // Apply the model's verdicts in call order. Coverage was validated above, so
        // every pending result finds its verdict; ids the model invented simply find
        // no home — the same harness-side guard applied to notification ids and
        // roster names below.
        let results = pending
            .iter()
            .map(|p| match bypassed(p) {
                // Decided by the shape rule before the fork was consulted
                // (docs/redaction-fork.md) — a `retain` entry for it, had the peek
                // written one unasked, finds no home.
                Some(shape) => bypass_verdict(shape),
                None => match verdict.results.iter().find(|v| v.call_id == p.call.id.0) {
                    Some(v) => Distillation {
                        wire: WireState::Distilled {
                            keep: v.keep.clone(),
                            note: v.note.clone(),
                        },
                        origin: Origin::Model,
                        raw_keep: v.raw_keep.clone(),
                    },
                    // Unreachable in practice (coverage validated); defensively
                    // withhold rather than keep untriaged output.
                    None => Distillation {
                        wire: WireState::TriageFailed {
                            why: "no verdict for this result".into(),
                        },
                        origin: Origin::Fallback,
                        raw_keep: String::new(),
                    },
                },
            })
            .collect();

        // Keep only dispositions for notifications we actually showed (drop any stray id
        // the model invented).
        let shown: HashSet<Seq> = inbox.iter().map(|n| n.seq).collect();
        let dispositions = verdict
            .inbox
            .into_iter()
            .filter(|(s, _)| shown.contains(s))
            .collect();
        // Same guard for background handles: only verdicts on processes we actually showed.
        let shown_bg: HashSet<&str> = background.iter().map(|b| b.handle.as_str()).collect();
        let bg = verdict
            .background
            .into_iter()
            .filter(|(h, _)| shown_bg.contains(h.as_str()))
            .collect();
        // Keep only names actually on the roster (drop any the model invented), deduped.
        let offered_names: HashSet<&str> = roster.iter().map(roster_name).collect();
        let mut kept = HashSet::new();
        let surfaced = verdict
            .surfaced
            .into_iter()
            .filter(|n| offered_names.contains(n.as_str()) && kept.insert(n.clone()))
            .collect();

        GateOutcome {
            results,
            inbox: dispositions,
            background: bg,
            surfaced,
            usage: record.usage,
            retries: record.retries,
            thinking: record.thinking,
        }
    }

    /// The standalone idle-triage peek (docs/supervisor.md, docs/inbox.md): fork off
    /// the cached prefix, show the inbox + background with no tool result, and ask for
    /// the `triage` tool via the tail instruction (no `keep` — there is no tool
    /// output). The caller wakes this when an event arrives while idle and escalates to
    /// a real turn iff it pins something. On any failure it pins/dismisses nothing —
    /// the agent simply goes back to idle, losing no data.
    pub async fn triage(
        &self,
        client: &Client,
        tools: &[Value],
        prefix: &[LogEntry],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> InboxTriage {
        let mut view = String::new();
        append_inbox_and_background(&mut view, inbox, background);
        view.push_str("\n\n");
        view.push_str(TRIAGE_INSTRUCTION);

        let mut messages = project_with(prefix, self.image_placement);
        place_trailing_cache_breakpoint(&mut messages);
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![ApiBlock::Text {
                text: view,
                cache_control: None,
            }],
        });

        let mut record = PeekRecord::default();
        let parsed = peek_with_retries(
            client,
            messages,
            tools,
            TRIAGE,
            |c| parse_dispositions(c, TRIAGE).ok_or_else(|| unusable(c, TRIAGE)),
            &mut record,
        )
        .await;

        match parsed {
            Ok((ibx, bg)) => {
                let shown: HashSet<Seq> = inbox.iter().map(|n| n.seq).collect();
                let shown_bg: HashSet<&str> =
                    background.iter().map(|b| b.handle.as_str()).collect();
                InboxTriage {
                    inbox: ibx.into_iter().filter(|(s, _)| shown.contains(s)).collect(),
                    background: bg
                        .into_iter()
                        .filter(|(h, _)| shown_bg.contains(h.as_str()))
                        .collect(),
                    origin: Origin::Model,
                    usage: record.usage,
                    retries: record.retries,
                    thinking: record.thinking,
                }
            }
            Err(why) => {
                log::warn!("idle triage peek produced no usable verdict; back to idle — {why}");
                InboxTriage {
                    inbox: Vec::new(),
                    background: Vec::new(),
                    origin: Origin::Fallback,
                    usage: record.usage,
                    retries: record.retries,
                    thinking: record.thinking,
                }
            }
        }
    }
}

// ---- the verdict ----

/// Triaged dispositions: inbox items (seq-keyed) and background processes (handle-keyed).
type Dispositions = (Vec<(Seq, Disposition)>, Vec<(String, BgVerdict)>);

// ---- the peek view ----

#[cfg(test)]
mod test;

mod handles;
mod parse;
mod prompt;
mod schema;

use handles::*;
use parse::*;
use prompt::*;

pub use schema::{retain_tool, triage_tool};
