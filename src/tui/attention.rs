//! Telling the host whether a person is attending this terminal (docs/web-push.md).
//!
//! The TUI reports `{client, session, active}` to the host's `POST /attention`, as
//! the web client does (`web/src/attention.ts`). [`Attender`] is the decision of
//! when a report is due: every input is passed in, so it runs without a terminal
//! or a network. The UI loop feeds it focus changes and input events and asks
//! [`Attender::due`] after each pass; a dispatcher task in `run.rs` sends what it
//! returns.

use std::time::{Duration, Instant};

use crate::daemon::attention::AttentionReport;

/// How recent an input event must be for the person to count as attending. A
/// person reading output without touching anything for longer lapses to the
/// host's 2-minute push escalation, which costs a delayed push at worst; a window
/// this long keeps someone following a long turn from being reported away.
pub(crate) const INPUT_WINDOW: Duration = Duration::from_secs(5 * 60);

/// The longest an active TUI goes without reporting. The host lets a client lapse
/// 45 s after its last report (`LAPSE`, `src/daemon/attention.rs`), so two beats
/// fit inside it.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(20);

/// A per-process id: one TUI process, one value, across the sessions it attaches.
pub(crate) fn process_client_id() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
    .clone()
}

struct Sent {
    active: bool,
    at: Instant,
}

/// The state machine behind reporting.
///
/// Active means focused and an input event within [`INPUT_WINDOW`]. Focus starts
/// out *focused*: the operator has just launched or attached the TUI, and a
/// terminal or multiplexer that never sends focus events (tmux without
/// `focus-events on`) leaves it reading focused, so input recency alone decides
/// there. Construction counts as input for the same reason.
///
/// A report is due when `active` differs from the last one sent, and when the
/// TUI is active and the last report is a [`HEARTBEAT`] old. `session` is fixed
/// for an attach (a session switch attaches afresh), so a change of it is a
/// new `Attender`. Once [`stop`](Self::stop)ped, nothing is due.
pub(crate) struct Attender {
    client: String,
    session: Option<String>,
    focused: bool,
    last_input: Option<Instant>,
    sent: Option<Sent>,
    stopped: bool,
}

impl Attender {
    pub(crate) fn new(client: String, session: Option<String>, now: Instant) -> Self {
        Self {
            client,
            session,
            focused: true,
            last_input: Some(now),
            sent: None,
            stopped: false,
        }
    }

    /// A key or mouse event arrived (`Moved` included).
    pub(crate) fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// The terminal reported `FocusGained` or `FocusLost`.
    pub(crate) fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Give up reporting: the host serves no attention table here (`404`), or its
    /// dispatcher is gone.
    pub(crate) fn stop(&mut self) {
        self.stopped = true;
    }

    fn active(&self, now: Instant) -> bool {
        self.focused
            && self
                .last_input
                .is_some_and(|at| now.saturating_duration_since(at) < INPUT_WINDOW)
    }

    fn report(&self, active: bool) -> AttentionReport {
        AttentionReport {
            client: self.client.clone(),
            session: self.session.clone(),
            active,
        }
    }

    /// The report to send now, if one is due; recorded as sent when returned.
    pub(crate) fn due(&mut self, now: Instant) -> Option<AttentionReport> {
        if self.stopped {
            return None;
        }
        let active = self.active(now);
        match &self.sent {
            Some(last) => {
                let changed = active != last.active;
                let beat = active && now.saturating_duration_since(last.at) >= HEARTBEAT;
                if !changed && !beat {
                    return None;
                }
            }
            // Nothing has been said and nothing is worth saying: the host treats
            // a client it has not heard from as inattentive already.
            None if !active => return None,
            None => {}
        }
        self.sent = Some(Sent { active, at: now });
        Some(self.report(active))
    }

    /// The last report, on leaving for good: inactive at once, so the host need
    /// not wait out the lapse. `None` when nothing active was ever said.
    pub(crate) fn leaving(&mut self) -> Option<AttentionReport> {
        if self.stopped || !self.sent.as_ref().is_some_and(|s| s.active) {
            return None;
        }
        self.sent = None;
        self.stopped = true;
        Some(self.report(false))
    }
}

#[cfg(test)]
mod test;
