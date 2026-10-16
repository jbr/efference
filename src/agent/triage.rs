//! Idle-wake triage — what the loop does when it parks with a pending inbox (docs/inbox.md).

use super::*;

use crate::process_monitor::BgView;

/// How the idle loop was woken (docs/process-monitor.md): a user turn, a blank line (keep idling),
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
            Some(t) if !t.is_blank() => IdleWake::Turn(t),
            Some(_) => IdleWake::Blank,
            None => IdleWake::Shutdown,
        }
    }
}

/// The standalone idle triage (docs/process-monitor.md — wake-on-notification). Drains the newly
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
    triager: &Triager,
    turn: &mut Turn,
) -> Result<bool, Error> {
    // Let a burst settle so several events triage together, not one triage each.
    tokio::time::sleep(std::time::Duration::from_millis(IDLE_TRIAGE_DEBOUNCE_MS)).await;

    let entries = shared.snapshot();
    let mut shown = active_shown(&entries);
    let mut forced = Vec::new();
    for drained in shared.drain_inbox() {
        let source = drained.notification.source.clone();
        let body = drained.notification.body.clone();
        let seq = shared.append(*turn, Payload::Ambient(drained.notification))?;
        shown.push(ShownNotification { seq, source, body });
        if drained.wake {
            forced.push(seq);
        }
    }
    let background = shared.bg_snapshot();
    // Nothing to look at (e.g. the waking notifications were dismissed out-of-band before
    // we drained) — back to idle without spending a model round-trip.
    if shown.is_empty() && background.is_empty() {
        return Ok(false);
    }
    let sight = Sight {
        entries: &entries,
        shown: &shown,
        forced: &forced,
        background: &background,
    };
    triage_and_record(client, tools, shared, triager, turn, sight).await
}

/// What a standalone triage looks at: the prefix it forks from, the notifications
/// and background tails it shows, and the must-wake seqs pinned whatever it says.
pub(super) struct Sight<'a> {
    pub entries: &'a [crate::log::LogEntry],
    pub shown: &'a [ShownNotification],
    pub forced: &'a [crate::log::Seq],
    pub background: &'a [BgView],
}

/// Run the tool-result-free triage over `shown` and `background`, apply its
/// dispositions, and log the verdict as a `Triage` with `for_result: None`. Returns
/// whether it pinned something. `entries` is the prefix the triage forks from, which must
/// already hold every result the wire owes. The shared tail of the idle wake and of the
/// triage an `ask` reply rides (which has no triage of its own, docs/process-monitor.md).
pub(super) async fn triage_and_record(
    client: &Client,
    tools: &[Value],
    shared: &Arc<Session>,
    triager: &Triager,
    turn: &mut Turn,
    sight: Sight<'_>,
) -> Result<bool, Error> {
    let Sight {
        entries,
        shown,
        forced,
        background,
    } = sight;
    let triage = triager
        .triage(client, tools, entries, shown, background)
        .await;
    for u in triage.usage {
        shared.record_request(RequestOrigin::Triage, u);
    }

    // Apply dispositions; escalate iff anything was pinned (the wire grew, docs/inbox.md).
    // A must-wake item is pinned whatever the triage said — that is what makes the
    // wake declared rather than judged, so a dismiss cannot swallow it.
    let mut inbox = triage.inbox;
    force_pins(&mut inbox, forced);
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
            // nudge is powered by proximity to work in flight (docs/triage.md).
            surfaced: Vec::new(),
            retries: triage.retries,
            thinking: triage.thinking,
        }),
    )?;
    Ok(escalate)
}
/// Override the triage's verdict for the must-wake notifications (docs/inbox.md): each
/// `forced` seq is pinned, whether the triage pinned it, dismissed it, or left it.
/// Deterministic on purpose — the fork keeps everything downstream of *whether* the
/// agent wakes (the note, the rest of the inbox), never the whether itself.
pub(super) fn force_pins(
    inbox: &mut Vec<(crate::log::Seq, Disposition)>,
    forced: &[crate::log::Seq],
) {
    if forced.is_empty() {
        return;
    }
    inbox.retain(|(seq, _)| !forced.contains(seq));
    inbox.extend(forced.iter().map(|seq| (*seq, Disposition::Pin)));
}
