//! The host's session listing, and the level-triggered stream of it
//! (docs/control-api.md).
//!
//! One resource, two readings of the same value. `GET /sessions` answers the
//! whole listing as JSON; the same request with `Accept: text/event-stream`
//! subscribes to it and receives the whole listing again on every change. There
//! is no durable class and no replay cursor here — a listing is a *current
//! value*, so a late subscriber is caught up by the next frame rather than by a
//! history, and a client that misses one frame is repaired by the next.
//!
//! The vocabulary is what the listing already answered — id, title, cwd,
//! created_by — plus two fields a list surface needs and cannot get without one
//! `GET /sessions/<id>/v1/state` per row: the session's [`status`] and how many
//! background processes it holds. The second is what a destructive affordance
//! reads: ending a session that holds processes reaps them, so a client asks
//! first (docs/tui.md, docs/web-client.md).
//!
//! [`status`]: SessionSummary::status

use super::super::status::SessionStatus;
use super::*;

/// One row of `GET /sessions` (docs/control-api.md). Also the shape the
/// `sessions` tool answers with, hence the schema derive (docs/tools.md).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The session's working directory, so a client can keep to its own
    /// project's sessions on a host that serves several
    /// (docs/control-channel.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The session that commissioned this one, when a session did
    /// (docs/tools.md). Absent for a human root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// What the session is doing, as a single word: `running`, `idle`,
    /// `gated`, or `stalled`.
    pub status: String,
    /// Live background processes the session holds (docs/supervisor.md). Ending
    /// it reaps them, which is why a listing carries the count.
    #[serde(default)]
    pub processes: usize,
}

/// The one word a row's `status` carries.
fn status_word(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "running",
        SessionStatus::Idle => "idle",
        SessionStatus::Gated { .. } => "gated",
        SessionStatus::Stalled { .. } => "stalled",
        // Neither is reachable from `Session::status`; the host reports an end
        // by removing the row (`status.rs`).
        SessionStatus::Ended => "ended",
        SessionStatus::Failed { .. } => "failed",
    }
}

impl Host {
    /// The listing now, sorted by id — the body of `GET /sessions` and the
    /// payload of each frame on its stream.
    pub fn summaries(&self) -> Vec<SessionSummary> {
        self.ids()
            .into_iter()
            .filter_map(|id| {
                let session = self.session(&id)?;
                let identity = session.identity();
                Some(SessionSummary {
                    title: identity.as_ref().and_then(|i| i.title.clone()),
                    cwd: identity.map(|i| i.cwd),
                    created_by: self.created_by(&id),
                    status: status_word(&session.status()).to_string(),
                    processes: session
                        .process_states()
                        .iter()
                        .filter(|process| process.background)
                        .count(),
                    id,
                })
            })
            .collect()
    }

    /// The listing as the bytes both readings send.
    pub(crate) fn summaries_json(&self) -> String {
        serde_json::to_string(&self.summaries()).unwrap_or_else(|_| "[]".to_string())
    }

    /// Signal that the listing may have moved. Cheap and idempotent, like the
    /// per-session `touch_state`: every subscriber recomputes and re-diffs, so
    /// an over-eager ping costs one comparison and a spurious one costs nothing.
    pub(crate) fn touch_summary(&self) {
        self.summary_changed.notify_waiters();
    }
}

/// One frame of the listing stream: the whole listing, as `event: sessions`.
///
/// No `id:` — there is no cursor to resume from, because there is no history to
/// resume into.
pub(crate) struct SummaryEvent(String);

impl Eventable for SummaryEvent {
    fn data(&self) -> Option<&str> {
        Some(&self.0)
    }
    fn event_type(&self) -> Option<&str> {
        Some("sessions")
    }
    fn id(&self) -> Option<&str> {
        None
    }
}

/// The per-connection listing stream. Emits the current listing immediately,
/// then re-emits it whenever it differs from what this connection last sent.
///
/// A `Weak` rather than an `Arc`: a stream a client left open must not be what
/// keeps the host alive, and the host going away is what ends it.
pub(crate) fn summary_stream(
    host: std::sync::Weak<Host>,
) -> Pin<Box<dyn Stream<Item = SummaryEvent> + Send + Sync + 'static>> {
    Box::pin(futures_lite::stream::unfold(
        (host, None::<String>),
        |(host, mut last)| async move {
            loop {
                let live = host.upgrade()?;
                // Armed before the listing is read, so a change landing between
                // the read and the await cannot be lost.
                let changed = live.summary_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let json = live.summaries_json();
                if last.as_deref() != Some(json.as_str()) {
                    last = Some(json.clone());
                    return Some((SummaryEvent(json), (host, last)));
                }
                changed.await;
            }
        },
    ))
}
