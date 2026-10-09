//! Whether anyone is attending to the host (docs/web-push.md).
//!
//! Clients report `{client, session, active}` to `POST /attention`; the host keeps
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
use crate::log::{AttentiveClient, PushCancel};

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

/// The most attentive clients a push record names.
const MAX_NAMED: usize = 8;

/// One client's report, as `POST /attention` receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttentionReport {
    /// A per-page-load (web) or per-process (TUI) id the client generated.
    pub(crate) client: String,
    /// The session the client has open, or `None` (null or absent) on a page
    /// that shows none (the session list).
    pub(crate) session: Option<String>,
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
            .session
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > MAX_ID_LEN)
        {
            return Err(format!(
                "bad attention report: session must be null or 1 to {MAX_ID_LEN} characters"
            ));
        }
        Ok(report)
    }
}

struct Latest {
    active: bool,
    session: Option<String>,
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
                session: report.session.clone(),
                at: now,
            },
        );
    }

    /// The unlapsed clients whose last report was `active`, judged at `now`,
    /// ordered by client id and capped at [`MAX_NAMED`] — what a held push
    /// records as the reason it was held (docs/web-push.md).
    pub(crate) fn attentive_at(&self, now: Instant) -> Vec<AttentiveClient> {
        let clients = self.clients.lock().unwrap();
        let mut attentive: Vec<AttentiveClient> = clients
            .iter()
            .filter(|(_, latest)| latest.active && now.duration_since(latest.at) < LAPSE)
            .map(|(client, latest)| AttentiveClient {
                client: client.clone(),
                session: latest.session.clone(),
            })
            .collect();
        attentive.sort_by(|a, b| a.client.cmp(&b.client));
        attentive.truncate(MAX_NAMED);
        attentive
    }

    pub(crate) fn attentive(&self) -> Vec<AttentiveClient> {
        self.attentive_at(Instant::now())
    }
}

impl Host {
    /// Take a client's report: record it, and when it is an active report from a
    /// client with a hosted session open, tell that session its pending moment has
    /// been seen ([`Session::cancel_escalation`]).
    pub(crate) fn report_attention(&self, report: &AttentionReport) {
        self.attention.report_at(report, Instant::now());
        if report.active
            && let Some(id) = &report.session
            && let Some(session) = self.session(id)
        {
            session.cancel_escalation(PushCancel::Seen {
                client: report.client.clone(),
            });
        }
    }
}

impl Session {
    /// Hand the session the host's attention table. Called once, by the host's
    /// adopt; a session outside a host has none and treats nobody as attentive.
    pub(crate) fn set_attention(&self, attention: Arc<Attention>) {
        let _ = self.attention.set(attention);
    }

    /// The clients attending, per the host's table; empty for a session outside
    /// a host.
    pub(super) fn attentive_clients(&self) -> Vec<AttentiveClient> {
        self.attention
            .get()
            .map(|a| a.attentive())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod test;
