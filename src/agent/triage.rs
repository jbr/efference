//! Idle-wake triage — what the loop does when it parks with a pending inbox (docs/inbox.md).

use super::*;

/// How the idle loop was woken (docs/supervisor.md): a user turn, a blank line (keep idling),
/// a notification event (run a standalone triage), a zero-token continue, or shutdown.
pub(super) enum IdleWake {
    Turn(SubmittedTurn),
    Blank,
    Event,
    /// `POST /loop/continue` (docs/control-api.md): control handed back with no
    /// input. Re-enter the trajectory with nothing appended — the same shape the
    /// escalated event takes, minus the pinned entry that woke it.
    Continue,
    Shutdown,
}

impl IdleWake {
    pub(super) fn from_turn(turn: Option<SubmittedTurn>) -> Self {
        match turn {
            Some(t) if !t.text.trim().is_empty() => IdleWake::Turn(t),
            Some(_) => IdleWake::Blank,
            None => IdleWake::Shutdown,
        }
    }
}

/// The standalone idle triage (docs/supervisor.md — wake-on-notification). Drains the newly
/// arrived notifications (logging them first-show) alongside the still-active ones and
/// the running background tails, runs the tool-result-free triage peek, applies the
/// dispositions, and logs the verdict as a `Triage` with `for_result: None`. Returns
/// **true** iff it pinned something — the escalation contract: a pin grows the wire, so
/// the caller runs a real turn to act on it; nothing pinned leaves the projection
/// byte-identical (audit-only), so the agent returns to idle with its cache still hot.
pub(super) async fn run_idle_triage(
    client: &Client,
    tools: &[Value],
    shared: &Arc<Session>,
    distiller: &ForkPeek,
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

    let triage = distiller
        .triage(client, tools, &entries, &shown, &background)
        .await;
    for u in triage.usage {
        shared.record_request(RequestOrigin::Triage, u);
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
                        Payload::Ambient(Notification {
                            source,
                            body,
                            slot: Some(handle),
                        }),
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
            // The idle triage rides no tool result and shows no surfacing roster — the
            // nudge is powered by proximity to work in flight (docs/redaction-fork.md).
            surfaced: Vec::new(),
            retries: triage.retries,
            thinking: triage.thinking,
        }),
    )?;
    Ok(escalate)
}
