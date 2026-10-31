//! Landing a batch's outcomes (docs/triage.md): the triage over its results, the
//! inbox triage that rides it, and the log entries that follow, in call order.

use super::*;

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
    // The must-wake notifications this triage first-shows (docs/inbox.md):
    // pinned below whatever the triage decides.
    let mut forced_pins: Vec<crate::log::Seq> = Vec::new();
    // The arrival mark the batch's last `Triage` records (docs/inbox.md): the highest
    // notification its rider showed, or `0` when the rider did not run here.
    let mut shown_through: crate::log::Seq = 0;
    let mut deferred: Option<(
        Vec<ShownNotification>,
        Vec<process_monitor::BgView>,
        Vec<crate::log::Seq>,
    )> = None;
    let mut triage = if pending.is_empty() {
        None
    } else {
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
        if crate::triage::reply_only_triage(&pending) {
            deferred = Some((shown.clone(), shared.bg_snapshot(), forced));
        } else {
            forced_pins = forced;
            // A batch the shape rule decided without the model shows no rider.
            if crate::triage::shows_rider(&pending) {
                shown_through = rider_mark(&shown);
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
        Some(loop {
            match triager
                .triage_results(
                    client,
                    tools,
                    &entries,
                    &pending,
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
                            break carried.interrupted(pending.len(), shared.interrupter());
                        }
                    }
                }
            }
        })
    };
    let triaged = !pending.is_empty();
    drop(pending);

    // The triage's usage: each fork request reads the shared canonical prefix
    // (sliding its TTL), so it belongs in the docs/log-and-projection.md telemetry.
    if let Some(g) = &mut triage {
        for u in &g.usage {
            shared.record_triage(!triager.is_own(), u.clone());
        }
        // The request the triage lineage's keepalive re-sends, where it re-sends a result
        // triage's (docs/transport.md).
        if let Some(sent) = g.sent.take() {
            shared.store_triage_request(sent.request, sent.at, &sent.usage);
        }
    }
    // The primer's usage and timing (docs/transport.md): its own
    // origin so `dev cache` prices the warmup separately, and one log line per triage
    // carrying the floor the triage paid (`residue_ms`) plus whether the spend had a
    // triage to warm. The residue distribution is what says if a speculative
    // (pre-execution) primer is ever worth building.
    if let Some(outcome) = primer {
        let primed = outcome.usage.is_some();
        if let Some(usage) = outcome.usage {
            shared.record_request(RequestOrigin::TriagePrimer, usage);
        }
        log::debug!(
            "triage primer: send {}ms, residue {}ms, primed {primed}, triaged {}",
            outcome.send_ms,
            outcome.residue_ms,
            triaged
        );
    }
    // Apply the background verdicts before writing the `Triage`s: a `Pin`
    // snapshots the process's current tail into an `Ambient` and folds a `Pin`
    // disposition into the inbox verdict — so it projects at the `Triage`
    // position, reusing the notification pin path (docs/process-monitor.md). A `Mute`
    // just flags the process monitor.
    let mut inbox_verdict = triage
        .as_mut()
        .map_or_default(|g| std::mem::take(&mut g.inbox));
    force_pins(&mut inbox_verdict, &forced_pins);
    if let Some(g) = &triage {
        for (handle, verdict) in &g.background {
            match verdict {
                BgVerdict::Mute => shared.mute_bg(handle),
                BgVerdict::Pin => {
                    if let Some((source, body)) = shared.bg_pin_snapshot(handle) {
                        let seq = shared.append(
                            turn,
                            Payload::Ambient(Notification::new(source, body, Some(handle.clone()))),
                        )?;
                        inbox_verdict.push((seq, Disposition::Pin));
                    }
                }
            }
        }
    }

    // Append in call order: each step's own payloads, and for a triaged result
    // its `ToolResult` followed by the `Triage` audit entry keyed to that
    // result's seq (docs/triage.md). The triage-scoped riders — inbox
    // dispositions and surfaced context — ride the **last** `Triage` of the batch:
    // they were decided once for the whole step, and tail-ward placement keeps the
    // wire append-only (docs/inbox.md), which is exactly where they already land
    // when a completion carried a single call.
    let mut verdicts = triage
        .as_mut()
        .map_or_default(|g| std::mem::take(&mut g.results))
        .into_iter();
    let surfaced = triage.as_ref().map_or_default(|g| g.surfaced.clone());
    // Triage-scoped like the inbox riders: the non-compliant-attempt breadcrumbs
    // land on the last `Triage` of the batch — durable even when a retry
    // recovered (docs/triage.md).
    let mut retries = triage
        .as_mut()
        .map_or_default(|g| std::mem::take(&mut g.retries));
    // Triage-scoped for the same reason: one triage reasoned about the whole batch,
    // so its thinking rides the last `Triage` rather than being copied per
    // result (docs/triage.md).
    let mut thinking = triage
        .as_mut()
        .map_or_default(|g| std::mem::take(&mut g.thinking));
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
                shared.append(
                    turn,
                    Payload::ResultVerdict {
                        for_result: result_seq,
                        wire,
                    },
                )?;
                let last = index == last_ran;
                shared.append(
                    turn,
                    Payload::Triage(Triage {
                        shown_through: Some(if last { shown_through } else { 0 }),
                        for_result: Some(result_seq),
                        origin,
                        raw_keep,
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
                        thinking: if last {
                            std::mem::take(&mut thinking)
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
    // The inbox an `ask` reply's triage could not triage (docs/process-monitor.md): the
    // results are in the log now, so the triage forks from a prefix that answers
    // every call.
    if let Some((shown, background, forced)) = deferred
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
            Payload::User(vec![ContentBlock::Text(format!(
                "[on backgrounding the command, the user says:] {note}"
            ))]),
        )?;
    }
    *turn_at = turn;
    Ok(())
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
        shared.append_sourced(
            *turn,
            Payload::User(vec![ContentBlock::Text(message)]),
            source,
        )?;
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
