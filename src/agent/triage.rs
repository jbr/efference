//! Idle-wake triage — what the loop does when it parks with a pending inbox (docs/inbox.md).

use super::*;

use crate::process_monitor::BgView;

/// Where a triage request's text and thinking go: the `output` class, as
/// `triage_delta` and `triage_thinking_delta` (docs/control-api.md). `call_ids` are the
/// results being triaged, empty for the idle triage. The tool-call fragments are dropped:
/// a triage's verdict lands as its `Triage` entries.
pub(super) fn triage_stream(
    shared: &Arc<Session>,
    call_ids: Vec<String>,
) -> impl Fn(Streamed) + Sync {
    let shared = Arc::clone(shared);
    move |streamed| match streamed {
        Streamed::Text { text, .. } => shared.publish_delta(StreamDelta::TriageText {
            call_ids: call_ids.clone(),
            text,
        }),
        Streamed::Thinking { text, .. } => shared.publish_delta(StreamDelta::TriageThinking {
            call_ids: call_ids.clone(),
            text,
        }),
        Streamed::ToolStart { .. } | Streamed::ToolInput { .. } => {}
    }
}

/// How the idle loop was woken (docs/process-monitor.md): a user turn, a blank line (keep idling),
/// a notification event (run a standalone triage), a zero-token continue, or shutdown.
pub(super) enum IdleWake {
    Turn(SubmittedTurn),
    Blank,
    Event,
    /// `POST /loop/continue` (docs/control-api.md): control handed back with no
    /// input. Re-enter the trajectory with nothing appended — the same shape the
    /// pinned-event wake takes, minus the pinned entry that woke it.
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

/// The standalone idle triage (docs/process-monitor.md — wake-on-notification). Opens the rider
/// (every unresolved notification, queued slot ones logged first-show) alongside the
/// running background tails, runs the tool-result-free triage peek, applies the
/// dispositions, and logs the verdict as a `Triage` with `for_result: None`. Returns
/// **true** iff it pinned something — the trajectory rule: a pin grows the wire, so
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

    let Rider {
        entries,
        shown,
        forced,
    } = open_rider(shared, *turn)?;
    let background = shared.bg_snapshot();
    // Nothing to look at — back to idle without spending a model round-trip.
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

/// What a triage's rider holds when it opens (docs/inbox.md): the prefix it forks from,
/// every unresolved notification, and the must-wake seqs among those no triage has shown.
pub(super) struct Rider {
    pub entries: Vec<crate::log::LogEntry>,
    pub shown: Vec<ShownNotification>,
    pub forced: Vec<crate::log::Seq>,
}

/// Open a triage's rider (docs/inbox.md): log the queued slot notifications as `Ambient`
/// entries, which is their first show, then fold the log. Notifications without a slot are
/// in the log from their arrival, so the fold holds them whether or not a triage has seen
/// them; the ones no triage has shown, carrying `wake`, are pinned whatever the verdict.
/// The `Ambient` entries project to nothing, so the fork prefix stays byte-identical
/// (docs/log-and-projection.md).
pub(super) fn open_rider(shared: &Arc<Session>, turn: Turn) -> Result<Rider, Error> {
    for queued in shared.drain_inbox() {
        shared.append(turn, Payload::Ambient(queued))?;
    }
    let entries = shared.snapshot();
    let shown = crate::inbox::rider(&entries);
    let forced = crate::inbox::unshown(&entries)
        .into_iter()
        .filter(|n| n.wake)
        .map(|n| n.seq)
        .collect();
    Ok(Rider {
        entries,
        shown,
        forced,
    })
}

/// The `shown_through` a triage records for the rider it showed: the highest arrival in it.
/// Everything the log held at or below that was in front of the model, and an arrival
/// logged while the request was in flight is above it (docs/inbox.md).
pub(super) fn rider_mark(shown: &[ShownNotification]) -> crate::log::Seq {
    shown.iter().map(|n| n.seq).max().unwrap_or(0)
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
        .triage(
            client,
            tools,
            entries,
            shown,
            background,
            &triage_stream(shared, Vec::new()),
        )
        .await;
    for u in triage.usage {
        shared.record_request(RequestOrigin::Triage, u);
    }

    // Apply dispositions; start a trajectory iff anything was pinned (the wire grew, docs/inbox.md).
    // A must-wake item is pinned whatever the triage said — that is what makes the
    // wake declared rather than judged, so a dismiss cannot swallow it.
    let mut inbox = triage.inbox;
    force_pins(&mut inbox, forced);
    let mut starts_trajectory = inbox.iter().any(|(_, d)| matches!(d, Disposition::Pin));
    for (handle, verdict) in triage.background {
        match verdict {
            BgVerdict::Mute => shared.mute_bg(&handle),
            BgVerdict::Pin => {
                if let Some((source, body)) = shared.bg_pin_snapshot(&handle) {
                    let seq = shared.append(
                        *turn,
                        Payload::Ambient(Notification::new(source, body, Some(handle))),
                    )?;
                    inbox.push((seq, Disposition::Pin));
                    starts_trajectory = true;
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
            shown_through: Some(rider_mark(shown)),
        }),
    )?;
    expire_unmentioned(shared, *turn)?;
    Ok(starts_trajectory)
}

/// Dismiss the notifications that [`MAX_UNMENTIONED_SHOWS`](crate::inbox::MAX_UNMENTIONED_SHOWS)
/// triages have shown without any verdict mentioning them (docs/inbox.md). Each gets a
/// `Telemetry::InboxExpired` entry, which projects to nothing and takes the notification
/// out of [`active_shown`](crate::inbox::active_shown). Run after a triage's entries are
/// written, so the count includes it.
pub(super) fn expire_unmentioned(shared: &Arc<Session>, turn: Turn) -> Result<(), Error> {
    for (seq, shows) in crate::inbox::unmentioned_past_cap(&shared.snapshot()) {
        shared.append(
            turn,
            Payload::Telemetry(crate::log::Telemetry::InboxExpired { seq, shows }),
        )?;
    }
    Ok(())
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
