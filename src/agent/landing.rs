//! Landing a batch's outcomes (docs/triage.md): the gate over its results, the
//! inbox triage that rides it, and the log entries that follow, in call order.

use super::*;

/// Triage the batch's results at one gate and append them to the log with their verdicts,
/// telemetry, and any handoff notes. Nothing to triage means no round-trip.
pub(super) async fn land(
    client: &Client,
    tools: &[Value],
    shared: &Arc<Session>,
    distiller: &ForkPeek,
    turn_at: &mut Turn,
    steps: Vec<Step>,
    primer: Option<TriagePrimerOutcome>,
) -> Result<(), Error> {
    let mut turn = *turn_at;
    // The gate (docs/triage.md): one peek over every result this
    // completion produced. Nothing to triage means no round-trip.
    let pending: Vec<Pending<'_>> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Ran { call, result, .. } => Some(Pending { call, raw: result }),
            Step::Direct => None,
        })
        .collect();
    // The must-wake notifications this gate first-shows (docs/inbox.md):
    // pinned below whatever the peek decides.
    let mut forced_pins: Vec<crate::log::Seq> = Vec::new();
    let mut deferred: Option<(
        Vec<ShownNotification>,
        Vec<crate::supervisor::BgView>,
        Vec<crate::log::Seq>,
    )> = None;
    let mut gate = if pending.is_empty() {
        None
    } else {
        // `ForkPeek` re-enters the model here — the prefix is the log through
        // this completion's `tool_use` blocks, so we snapshot it (can't borrow
        // across the distiller's await). A peek on the agent's client reads the
        // main prefix, so it waits at the cold check like a turn's request.
        if !distiller.is_own() {
            shared.await_warm_or_person().await;
        }
        let mut entries = shared.snapshot();
        // Open the triage gate (docs/inbox.md): re-show the still-active
        // notifications (left at earlier gates), then log any newly arrived
        // (ephemeral) ones as first-show `Ambient` entries — each entry's seq
        // becomes its durable id, the handle the verdict pins/dismisses by. (They
        // project to nothing, so the fork prefix `entries` stays byte-identical,
        // docs/log-and-projection.md.)
        let mut shown = active_shown(&entries);
        let mut forced = Vec::new();
        for drained in shared.drain_inbox() {
            let source = drained.notification.source.clone();
            let body = drained.notification.body.clone();
            let seq = shared.append(turn, Payload::Ambient(drained.notification))?;
            shown.push(ShownNotification { seq, source, body });
            if drained.wake {
                forced.push(seq);
            }
        }
        // A gate that is only `ask` replies has no peek to carry the inbox
        // (docs/supervisor.md): triage runs after the results are logged, below,
        // and takes the must-wake pins with it.
        if crate::triage::reply_only_gate(&pending) {
            deferred = Some((shown.clone(), shared.bg_snapshot(), forced));
        } else {
            forced_pins = forced;
        }
        // The live background processes' fresh tails ride the same peek
        // (docs/supervisor.md) — re-sampled now, never logged unless pinned.
        let background = shared.bg_snapshot();
        // A gate without a verdict holds rather than reaching the agent
        // (docs/triage.md): the stall names it, and the operator's
        // continue runs the gate again from the top.
        let mut carried = Carried::default();
        Some(loop {
            match distiller
                .triage_results(client, tools, &entries, &pending, &shown, &background)
                .await
            {
                Ok(outcome) => break carried.onto(outcome),
                Err(failure) => {
                    match hold(shared, client, !distiller.is_own(), failure, &mut carried).await {
                        // Re-read the log: a fold written during the hold
                        // (docs/summary-head.md) is what the retried peek forks, not
                        // the history it replaced. The `Ambient` entries appended
                        // above project to nothing, so the prefix is otherwise the same.
                        Held::Retry => {
                            entries = shared.snapshot();
                            continue;
                        }
                        Held::Interrupted => break carried.interrupted(pending.len()),
                    }
                }
            }
        })
    };
    let gated = !pending.is_empty();
    drop(pending);

    // The gate's usage: each fork request reads the shared canonical prefix
    // (sliding its TTL), so it belongs in the docs/log-and-projection.md telemetry.
    if let Some(g) = &gate {
        for u in &g.usage {
            shared.record_peek(!distiller.is_own(), u.clone());
        }
    }
    // The primer's usage and timing (docs/transport.md): its own
    // origin so `dev cache` prices the warmup separately, and one log line per gate
    // carrying the floor the gate paid (`residue_ms`) plus whether the spend had a
    // gate to warm. The residue distribution is what says if a speculative
    // (pre-execution) primer is ever worth building.
    if let Some(outcome) = primer {
        let primed = outcome.usage.is_some();
        if let Some(usage) = outcome.usage {
            shared.record_request(RequestOrigin::TriagePrimer, usage);
        }
        log::debug!(
            "triage primer: send {}ms, residue {}ms, primed {primed}, gated {}",
            outcome.send_ms,
            outcome.residue_ms,
            gated
        );
    }
    // Apply the background verdicts before writing the `Triage`s: a `Pin`
    // snapshots the process's current tail into an `Ambient` and folds a `Pin`
    // disposition into the inbox verdict — so it projects at the `Triage`
    // position, reusing the notification pin path (docs/supervisor.md). A `Mute`
    // just flags the supervisor.
    let mut inbox_verdict = gate
        .as_mut()
        .map(|g| std::mem::take(&mut g.inbox))
        .unwrap_or_default();
    force_pins(&mut inbox_verdict, &forced_pins);
    if let Some(g) = &gate {
        for (handle, verdict) in &g.background {
            match verdict {
                BgVerdict::Mute => shared.mute_bg(handle),
                BgVerdict::Pin => {
                    if let Some((source, body)) = shared.bg_pin_snapshot(handle) {
                        let seq = shared.append(
                            turn,
                            Payload::Ambient(Notification {
                                source,
                                body,
                                slot: Some(handle.clone()),
                            }),
                        )?;
                        inbox_verdict.push((seq, Disposition::Pin));
                    }
                }
            }
        }
    }

    // Append in call order: each step's own payloads, and for a triaged result
    // its `ToolResult` followed by the `Triage` audit entry keyed to that
    // result's seq (docs/triage.md). The gate-scoped riders — inbox
    // dispositions and surfaced context — ride the **last** `Triage` of the batch:
    // they were decided once for the whole step, and tail-ward placement keeps the
    // wire append-only (docs/inbox.md), which is exactly where they already land
    // when a completion carried a single call.
    let mut verdicts = gate
        .as_mut()
        .map(|g| std::mem::take(&mut g.results))
        .unwrap_or_default()
        .into_iter();
    let surfaced = gate
        .as_ref()
        .map(|g| g.surfaced.clone())
        .unwrap_or_default();
    // Gate-scoped like the inbox riders: the non-compliant-attempt breadcrumbs
    // land on the last `Triage` of the batch — durable even when a retry
    // recovered (docs/triage.md).
    let mut retries = gate
        .as_mut()
        .map(|g| std::mem::take(&mut g.retries))
        .unwrap_or_default();
    // Gate-scoped for the same reason: one peek reasoned about the whole batch,
    // so its thinking rides the last `Triage` rather than being copied per
    // result (docs/triage.md).
    let mut thinking = gate
        .as_mut()
        .map(|g| std::mem::take(&mut g.thinking))
        .unwrap_or_default();
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
                // A gate that answered for fewer results than it was offered would leave a
                // captured result pending forever, which projects to nothing and orphans its
                // `tool_use` on the next request (docs/README.md). The distiller does not do
                // that — one result short of a verdict is a `GateFailure`, which holds or
                // withholds the whole batch — so this is the breach's last line of defence:
                // withhold the result, the same way an interrupted gate does, and say so in
                // the log rather than leaving a silent hole.
                let (wire, origin, raw_keep) = match verdicts.next() {
                    Some(verdict) => (verdict.wire, verdict.origin, verdict.raw_keep),
                    None => {
                        log::error!(
                            "the gate answered for fewer results than the batch captured; \
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
    // The inbox an `ask` reply's gate could not triage (docs/supervisor.md): the
    // results are in the log now, so the peek forks from a prefix that answers
    // every call.
    if let Some((shown, background, forced)) = deferred
        && (!shown.is_empty() || !background.is_empty() || !forced.is_empty())
    {
        let entries = shared.snapshot();
        triage_and_record(
            client,
            tools,
            shared,
            distiller,
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
    // The operator note attached to a handoff lands as a real user turn after the
    // step's results — direction, not tool output (docs/supervisor.md). Same
    // canonical `Payload::User` as the seam-drain; `project` merges it into the
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
    redirect: Option<String>,
) -> std::io::Result<bool> {
    if abort_turn {
        log::debug!("turn aborted");
        return Ok(true);
    }
    if let Some(message) = redirect {
        // The rejection `tool_result`(s) answer the orphaned call(s); this
        // user turn carries the steer. The trajectory goes on: re-project and let
        // the model respond.
        *turn += 1;
        shared.append(*turn, Payload::User(vec![ContentBlock::Text(message)]))?;
    }
    // The juncture after the gate (docs/control-channel.md): an interrupt
    // pressed during the peek or a held gate ended it with the latch still
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
