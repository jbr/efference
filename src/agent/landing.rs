//! Landing a batch's outcomes (docs/triage.md): the triage over its results, the
//! inbox triage that rides it, and the log entries that follow, in call order.

use super::*;
use crate::triage::{HoldOutcome, TriageOutcome};

/// Triage the batch's results at one triage and append them to the log with their verdicts,
/// telemetry, and any handoff notes. Nothing to triage means no round-trip.
pub(super) async fn land(
    client: &Client,
    tools: &[Value],
    shared: &Arc<Session>,
    triager: &Triager,
    turn_at: &mut Turn,
    steps: Vec<Step>,
    primer: Option<TriagePrimerOutcome>,
) -> Result<(), Error> {
    let mut turn = *turn_at;
    // The triage (docs/triage.md): one triage over every result this
    // completion produced. Nothing to triage means no round-trip.
    let pending: Vec<Pending<'_>> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Ran { call, result, .. } => Some(Pending { call, raw: result }),
            Step::Direct => None,
        })
        .collect();
    let Triaging {
        outcome,
        forced_pins,
        shown_through,
        deferred,
    } = triage_batch(client, tools, shared, triager, turn, &pending).await?;
    let triaged = !pending.is_empty();
    drop(pending);

    let mut outcome = outcome.unwrap_or_default();
    record_triage_usage(shared, triager, &mut outcome);
    if let Some(primer) = primer {
        record_primer_usage(shared, primer, triaged);
    }
    let inbox = decide_inbox(shared, turn, &mut outcome, &forced_pins)?;
    let riders = Riders {
        shown_through,
        inbox,
        surfaced: outcome.surfaced,
        retries: outcome.retries,
        thinking: outcome.thinking,
    };
    let handoffs = append_in_call_order(shared, turn, steps, outcome.results, riders)?;
    // The inbox an `ask` reply's triage could not triage (docs/process-monitor.md): the
    // results are in the log now, so the triage forks from a prefix that answers
    // every call.
    if let Some(Deferred {
        shown,
        background,
        forced,
    }) = deferred
        && (!shown.is_empty() || !background.is_empty() || !forced.is_empty())
    {
        let entries = shared.snapshot();
        triage_and_record(
            client,
            tools,
            shared,
            triager,
            &mut turn,
            Sight {
                entries: &entries,
                shown: &shown,
                forced: &forced,
                background: &background,
            },
        )
        .await?;
    }
    expire_unmentioned(shared, turn)?;
    // The operator note attached to a handoff lands as a real user turn after the
    // step's results — direction, not tool output (docs/process-monitor.md). Same
    // canonical `Payload::User` as a queued turn delivered mid-task; `project` merges it into the
    // trailing tool_result message.
    for note in handoffs {
        turn += 1;
        shared.append(
            turn,
            Payload::user(format!(
                "[on backgrounding the command, the user says:] {note}"
            )),
        )?;
    }
    *turn_at = turn;
    Ok(())
}

/// What triaging a batch's results decided, and what it leaves for landing them.
struct Triaging {
    /// The triage's outcome; `None` when no result needed one.
    outcome: Option<HoldOutcome>,
    /// The must-wake notifications this triage first-shows (docs/inbox.md):
    /// pinned below whatever the triage decides.
    forced_pins: Vec<crate::record::log::Seq>,
    /// The arrival mark the batch's last `Triage` records (docs/inbox.md): the highest
    /// notification its rider showed, or `0` when the rider did not run here.
    shown_through: crate::record::log::Seq,
    /// The rider a triage of `ask` replies alone could not carry (docs/process-monitor.md).
    deferred: Option<Deferred>,
}

/// An inbox left for the triage that follows the batch's results.
struct Deferred {
    shown: Vec<ShownNotification>,
    background: Vec<process_monitor::BgView>,
    forced: Vec<crate::record::log::Seq>,
}

/// Run the triage over the batch's results (docs/triage.md), holding it until it has a
/// verdict. `pending` is empty when nothing needs one.
async fn triage_batch(
    client: &Client,
    tools: &[Value],
    shared: &Arc<Session>,
    triager: &Triager,
    turn: Turn,
    pending: &[Pending<'_>],
) -> Result<Triaging, Error> {
    let mut triaging = Triaging {
        outcome: None,
        forced_pins: Vec::new(),
        shown_through: 0,
        deferred: None,
    };
    if pending.is_empty() {
        return Ok(triaging);
    }
    // `Triager` re-enters the model here — the prefix is the log through
    // this completion's `tool_use` blocks, so we snapshot it (can't borrow
    // across the triager's await). A triage on the agent's client reads the
    // main prefix, so it waits at the cold check like a turn's request.
    if !triager.is_own() {
        shared.await_warm_or_person().await;
    }
    shared.set_phase(LoopPhase::Requesting);
    // Open the triage (docs/inbox.md): the rider holds every unresolved
    // notification, those left at earlier triages and those that arrived since,
    // queued slot notifications logged now as their first show. Each entry's seq
    // is the handle the verdict pins/dismisses by. (They project to nothing, so the
    // fork prefix `entries` stays byte-identical, docs/log-and-projection.md.)
    let Rider {
        mut entries,
        shown,
        forced,
    } = open_rider(shared, turn)?;
    // A triage that is only `ask` replies has no triage to carry the inbox
    // (docs/process-monitor.md): triage runs after the results are logged, below,
    // and takes the must-wake pins with it.
    if crate::triage::reply_only_triage(pending) {
        triaging.deferred = Some(Deferred {
            shown: shown.clone(),
            background: shared.bg_snapshot(),
            forced,
        });
    } else {
        triaging.forced_pins = forced;
        // A batch the shape rule decided without the model shows no rider.
        if crate::triage::shows_rider(pending) {
            triaging.shown_through = rider_mark(&shown);
        }
    }
    // The live background processes' fresh tails ride the same triage
    // (docs/process-monitor.md) — re-sampled now, never logged unless pinned.
    let background = shared.bg_snapshot();
    // A triage without a verdict holds rather than reaching the agent
    // (docs/triage.md): the stall names it, and the operator's
    // continue runs the triage again from the top.
    let mut carried = Carried::default();
    let on_delta = triage_stream(
        shared,
        pending.iter().map(|p| p.call.id.0.clone()).collect(),
    );
    triaging.outcome = Some(loop {
        match triager
            .triage_results(
                client,
                tools,
                &entries,
                pending,
                &shown,
                &background,
                &on_delta,
            )
            .await
        {
            Ok(outcome) => break carried.onto(outcome),
            Err(failure) => {
                match hold(shared, client, !triager.is_own(), failure, &mut carried).await {
                    // Re-read the log: a compaction written during the hold
                    // (docs/summary.md) is what the retried triage forks, not
                    // the history it replaced. The `Ambient` entries appended
                    // above project to nothing, so the prefix is otherwise the same.
                    Held::Retry => {
                        entries = shared.snapshot();
                        continue;
                    }
                    Held::Interrupted => {
                        break carried.interrupted(pending.len(), shared.interrupter().as_deref());
                    }
                }
            }
        }
    });
    Ok(triaging)
}

/// The triage's usage: each fork request reads the shared canonical prefix
/// (sliding its TTL), so it belongs in the docs/log-and-projection.md telemetry.
fn record_triage_usage(shared: &Session, triager: &Triager, outcome: &mut HoldOutcome) {
    for u in &outcome.usage {
        shared.record_triage(!triager.is_own(), u.clone());
    }
    // The request the triage lineage's keepalive re-sends, where it re-sends a result
    // triage's (docs/transport.md).
    if let Some(sent) = outcome.sent.take() {
        shared.store_triage_request(sent.request, sent.at, &sent.usage);
    }
}

/// The primer's usage and timing (docs/transport.md): its own
/// origin so `dev cache` prices the warmup separately, and one log line per triage
/// carrying the floor the triage paid (`residue_ms`) plus whether the spend had a
/// triage to warm. The residue distribution is what says if a speculative
/// (pre-execution) primer is ever worth building.
fn record_primer_usage(shared: &Session, primer: TriagePrimerOutcome, triaged: bool) {
    let primed = primer.usage.is_some();
    if let Some(usage) = primer.usage {
        shared.record_request(RequestOrigin::TriagePrimer, usage);
    }
    log::debug!(
        "triage primer: send {}ms, residue {}ms, primed {primed}, triaged {}",
        primer.send_ms,
        primer.residue_ms,
        triaged
    );
}

/// The inbox verdict the batch's last `Triage` carries (docs/inbox.md): the triage's own,
/// the must-wake pins over it, and the pins its background verdicts add.
///
/// The background verdicts apply before the `Triage`s are written: a `Pin`
/// snapshots the process's current tail into an `Ambient` and folds a `Pin`
/// disposition into the inbox verdict — so it projects at the `Triage`
/// position, reusing the notification pin path (docs/process-monitor.md). A `Mute`
/// just flags the process monitor.
fn decide_inbox(
    shared: &Session,
    turn: Turn,
    outcome: &mut HoldOutcome,
    forced_pins: &[crate::record::log::Seq],
) -> std::io::Result<Vec<(crate::record::log::Seq, Disposition)>> {
    let mut inbox = std::mem::take(&mut outcome.inbox);
    force_pins(&mut inbox, forced_pins);
    for (handle, verdict) in &outcome.background {
        match verdict {
            BgVerdict::Mute => shared.mute_bg(handle),
            BgVerdict::Pin => {
                if let Some((source, body)) = shared.bg_pin_snapshot(handle) {
                    let seq = shared.append(
                        turn,
                        Payload::Ambient(Notification::new(source, body, Some(handle.clone()))),
                    )?;
                    inbox.push((seq, Disposition::Pin));
                }
            }
        }
    }
    Ok(inbox)
}

/// What one triage decided for the whole step rather than per result: it rides the
/// **last** `Triage` of the batch, and tail-ward placement keeps the wire append-only
/// (docs/inbox.md), which is exactly where it already lands when a completion carried a
/// single call.
#[derive(Default)]
struct Riders {
    /// The batch's arrival mark (docs/inbox.md).
    shown_through: crate::record::log::Seq,
    /// The inbox dispositions.
    inbox: Vec<(crate::record::log::Seq, Disposition)>,
    /// The surfaced context.
    surfaced: Vec<String>,
    /// The non-compliant-attempt breadcrumbs, durable even when a retry recovered
    /// (docs/triage.md).
    retries: Vec<String>,
    /// One triage reasoned about the whole batch, so its thinking rides the last `Triage`
    /// rather than being copied per result (docs/triage.md).
    thinking: Vec<String>,
}

/// Append in call order: each step's own payloads, and for a triaged result
/// its `ToolResult` verdict followed by the `Triage` audit entry keyed to that
/// result's seq (docs/triage.md). The triage-scoped `riders` ride the last `Triage`.
/// Returns the handoff notes the steps carried.
fn append_in_call_order(
    shared: &Session,
    turn: Turn,
    steps: Vec<Step>,
    results: Vec<TriageOutcome>,
    mut riders: Riders,
) -> std::io::Result<Vec<String>> {
    let mut verdicts = results.into_iter();
    let last_ran = steps
        .iter()
        .rposition(|s| matches!(s, Step::Ran { .. }))
        .unwrap_or(0);
    let mut handoffs: Vec<String> = Vec::new();
    for (index, step) in steps.into_iter().enumerate() {
        match step {
            Step::Direct => {
                // Its payloads went down as the step was decided, with a verdict already
                // in them or none needed (docs/triage.md).
            }
            Step::Ran {
                result_seq,
                handoff,
                ..
            } => {
                // The verdict, appended against the result it decides (docs/triage.md):
                // the output is on the log from the moment it existed, so what lands now is
                // the decision alone and the two can never disagree about it.
                //
                // A triage that answered for fewer results than it was offered would leave a
                // captured result pending forever, which projects to nothing and orphans its
                // `tool_use` on the next request (docs/README.md). The triager does not do
                // that — one result short of a verdict is a `HoldFailure`, which holds or
                // withholds the whole batch — so this is the breach's last line of defence:
                // withhold the result, the same way an interrupted triage does, and say so in
                // the log rather than leaving a silent hole.
                let (wire, origin, raw_keep) = match verdicts.next() {
                    Some(verdict) => (verdict.wire, verdict.origin, verdict.raw_keep),
                    None => {
                        log::error!(
                            "the triage answered for fewer results than the batch captured; \
                             one result is withheld from the wire"
                        );
                        (WireState::Elided, Origin::Fallback, String::new())
                    }
                };
                shared.append(turn, Payload::result_verdict(result_seq, wire))?;
                let riders = if index == last_ran {
                    std::mem::take(&mut riders)
                } else {
                    Riders::default()
                };
                shared.append(
                    turn,
                    Payload::Triage(Triage {
                        shown_through: Some(riders.shown_through),
                        for_result: Some(result_seq),
                        origin,
                        raw_keep,
                        inbox: riders.inbox,
                        surfaced: riders.surfaced,
                        retries: riders.retries,
                        thinking: riders.thinking,
                    }),
                )?;
                if let Some(note) = handoff {
                    handoffs.push(note);
                }
            }
        }
    }
    Ok(handoffs)
}

/// What follows a landed batch (docs/control-channel.md): whether the trajectory ends
/// (`true`), and the redirect's user turn when it goes on. The caller breaks out of its
/// loop on `true`.
pub(super) fn after_landing(
    shared: &Session,
    turn: &mut Turn,
    abort_turn: bool,
    redirect: Option<Redirect>,
) -> std::io::Result<bool> {
    if abort_turn {
        log::debug!("turn aborted");
        return Ok(true);
    }
    if let Some(Redirect { message, source }) = redirect {
        // The rejection `tool_result`(s) answer the orphaned call(s); this
        // user turn carries the steer, under its decider's source, so it opens as that
        // decider's messages do (docs/permissions.md). The trajectory goes on:
        // re-project and let the model respond.
        *turn += 1;
        shared.append_sourced(*turn, Payload::user(message), source)?;
    }
    // The juncture after the triage (docs/control-channel.md): an interrupt
    // pressed during the triage or a held triage ended it with the latch still
    // set. Consumed here, the turn ends with the results logged withheld;
    // left set, the next completion's first call would take it, after a
    // paid request and after any turn the operator queued to explain the
    // press had already been drained into that request.
    if shared.take_interrupt() {
        log::debug!("turn ended by the operator's interrupt");
        return Ok(true);
    }
    Ok(false)
}
