//! Whether anyone is attending to the host (docs/web-push.md).
//!
//! Clients report `{client, viewing, active}` to `POST /attention`; the host keeps
//! the latest report per client with its arrival time. A client **lapses** when its
//! reports stop, and someone is **attentive** when any unlapsed client's last
//! report said `active`. The state is host-wide: one [`Attention`] is shared by
//! every session the host adopts ([`Session::set_attention`]), so a push for any
//! session reads what every client has said, wherever it is looking.
//!
//! Nothing here is tied to a connection. A client that dies holding `active`
//! stops counting [`LAPSE`] after its last report, and a client that goes idle
//! says so and stops counting at once.

use super::*;

/// How long after its last report a client still counts. The web client
/// heartbeats about every 20 seconds while active, so two missed beats lapse it:
/// long enough that one slow request does not read as inattention, short enough
/// that a closed laptop or a killed tab stops suppressing the immediate push
/// within the minute. A lapse only costs a push that would otherwise have been
/// held back for the escalation delay.
pub(crate) const LAPSE: Duration = Duration::from_secs(45);

/// The longest client id or session id a report may carry. Both are short
/// identifiers; the cap keeps a malformed client from filling the table.
const MAX_ID_LEN: usize = 128;

/// One client's report, as `POST /attention` receives it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttentionReport {
    /// A per-page-load id the client generated: one tab, one value.
    pub(crate) client: String,
    /// The session the client has open, or `None` (null or absent) on a page
    /// that shows none (the session list).
    pub(crate) viewing: Option<String>,
    /// Whether the person is attending: the page is visible, focused, and has
    /// seen recent input.
    pub(crate) active: bool,
}

impl AttentionReport {
    /// Parse a request body, or say what was wrong with it.
    pub(crate) fn parse(body: &str) -> Result<Self, String> {
        let report: Self =
            serde_json::from_str(body).map_err(|e| format!("bad attention report: {e}"))?;
        if report.client.is_empty() || report.client.len() > MAX_ID_LEN {
            return Err(format!(
                "bad attention report: client must be 1 to {MAX_ID_LEN} characters, got {}",
                report.client.len()
            ));
        }
        if report
            .viewing
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > MAX_ID_LEN)
        {
            return Err(format!(
                "bad attention report: viewing must be null or 1 to {MAX_ID_LEN} characters"
            ));
        }
        Ok(report)
    }
}

struct Latest {
    active: bool,
    at: Instant,
}

/// The host's attention table: the latest report per client.
#[derive(Default)]
pub(crate) struct Attention {
    clients: Mutex<HashMap<String, Latest>>,
}

impl Attention {
    /// Record a report that arrived at `now`, dropping clients that have lapsed.
    pub(crate) fn report_at(&self, report: &AttentionReport, now: Instant) {
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|_, latest| now.duration_since(latest.at) < LAPSE);
        clients.insert(
            report.client.clone(),
            Latest {
                active: report.active,
                at: now,
            },
        );
    }

    /// Whether any unlapsed client's last report was `active`, judged at `now`.
    pub(crate) fn attentive_at(&self, now: Instant) -> bool {
        self.clients
            .lock()
            .unwrap()
            .values()
            .any(|latest| latest.active && now.duration_since(latest.at) < LAPSE)
    }

    pub(crate) fn attentive(&self) -> bool {
        self.attentive_at(Instant::now())
    }
}

impl Host {
    /// Take a client's report: record it, and when it is an active report from a
    /// client viewing a hosted session, tell that session its pending moment has
    /// been seen ([`Session::cancel_escalation`]).
    pub(crate) fn report_attention(&self, report: &AttentionReport) {
        self.attention.report_at(report, Instant::now());
        if report.active
            && let Some(id) = &report.viewing
            && let Some(session) = self.session(id)
        {
            session.cancel_escalation();
        }
    }
}

impl Session {
    /// Hand the session the host's attention table. Called once, by the host's
    /// adopt; a session outside a host has none and treats nobody as attentive.
    pub(crate) fn set_attention(&self, attention: Arc<Attention>) {
        let _ = self.attention.set(attention);
    }

    /// Whether any client is attending, per the host's table.
    pub(super) fn someone_is_attentive(&self) -> bool {
        self.attention.get().is_some_and(|a| a.attentive())
    }
}

#[cfg(test)]
mod test;
