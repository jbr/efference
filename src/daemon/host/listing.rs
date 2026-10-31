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
//! [`status`]: ListedSession::status

use super::super::status::SessionStatus;
use super::*;

/// The one word a row's `status` carries.
impl From<&SessionStatus> for ListedStatus {
    // `Idle` and `Ended | Failed` reach the same word for unrelated reasons: one is the
    // status itself, the other is unreachable here and stands in only so the match is
    // exhaustive.
    #[allow(clippy::match_same_arms)]
    fn from(status: &SessionStatus) -> Self {
        match status {
            SessionStatus::Running => Self::Running,
            SessionStatus::Idle => Self::Idle,
            SessionStatus::Gated { .. } => Self::Gated,
            SessionStatus::Asking { .. } => Self::Asking,
            SessionStatus::Stalled { .. } => Self::Stalled,
            SessionStatus::Parked { .. } => Self::Parked,
            SessionStatus::Blocked { .. } => Self::Blocked,
            // Neither is reachable from `Session::status`; the host reports an end
            // by removing the row (`status.rs`), so no word names them.
            SessionStatus::Ended | SessionStatus::Failed { .. } => Self::Idle,
        }
    }
}

impl Host {
    /// The listing now, sorted by id — the body of `GET /sessions` and the
    /// payload of each frame on its stream.
    pub fn listing(&self) -> Vec<ListedSession> {
        self.ids()
            .into_iter()
            .filter_map(|id| self.listed(&id))
            .collect()
    }

    /// One session's row — what `GET /sessions/:id` answers and what the listing
    /// holds for it. `None` when this host is neither running nor starting `id`. A
    /// starting session's row says `starting`, and has no title or processes yet.
    pub fn listed(&self, id: &str) -> Option<ListedSession> {
        let session = match self.entry(id)? {
            Entry::Hosted(hosted) => hosted.session,
            Entry::Starting(starting) => {
                let project = starting.project();
                return Some(ListedSession {
                    id: id.to_string(),
                    title: None,
                    cwd: Some(project.to_string_lossy().into_owned()),
                    working: (starting.working() != project)
                        .then(|| starting.working().to_string_lossy().into_owned()),
                    created_by: starting.created_by().map(str::to_string),
                    status: ListedStatus::Starting,
                    processes: 0,
                });
            }
        };
        let identity = session.identity();
        let directories = session.directories();
        Some(ListedSession {
            title: identity.as_ref().and_then(|i| i.title.clone()),
            cwd: Some(directories.project().to_string_lossy().into_owned()),
            working: (directories.working() != directories.project())
                .then(|| directories.working().to_string_lossy().into_owned()),
            created_by: self.created_by(id),
            status: ListedStatus::from(&session.status()),
            processes: session
                .process_states()
                .iter()
                .filter(|process| process.background)
                .count(),
            id: id.to_string(),
        })
    }

    /// The listing as the bytes both readings send.
    pub(crate) fn listing_json(&self) -> String {
        serde_json::to_string(&self.listing()).unwrap_or_else(|_| "[]".to_string())
    }

    /// Signal that the listing may have moved. Cheap and idempotent, like the
    /// per-session `touch_state`: every subscriber recomputes and re-diffs, so
    /// an over-eager ping costs one comparison and a spurious one costs nothing.
    pub(crate) fn touch_listing(&self) {
        self.listing_changed.notify_waiters();
    }

    /// Keep the terminal title of the terminal this host runs in at its session
    /// count until the host shuts down, then put back the one it found
    /// (docs/operating.md). Another reader of the listing signal: a status wake
    /// that leaves the count alone writes nothing.
    pub async fn title_terminal(self: Arc<Self>, mut out: impl io::Write) {
        use crate::terminal_title::{self, TerminalTitle};
        let _ = terminal_title::save(&mut out);
        let mut title = TerminalTitle::default();
        loop {
            // Armed before the count is read, as in `listing_stream`.
            let changed = self.listing_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let _ = title.show(&mut out, &terminal_title::for_host(self.ids().len()));
            if self.swansong.interrupt(changed).await.is_none() {
                break;
            }
        }
        let _ = terminal_title::restore(&mut out);
    }
}

/// One frame of the listing stream: the whole listing, as `event: sessions`.
///
/// No `id:` — there is no cursor to resume from, because there is no history to
/// resume into.
pub(crate) struct ListingEvent(String);

impl Eventable for ListingEvent {
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
pub(crate) fn listing_stream(
    host: std::sync::Weak<Host>,
) -> Pin<Box<dyn Stream<Item = ListingEvent> + Send + Sync + 'static>> {
    Box::pin(futures_lite::stream::unfold(
        (host, None::<String>),
        |(host, mut last)| async move {
            loop {
                let live = host.upgrade()?;
                // Armed before the listing is read, so a change landing between
                // the read and the await cannot be lost.
                let changed = live.listing_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let json = live.listing_json();
                if last.as_deref() != Some(json.as_str()) {
                    last = Some(json.clone());
                    return Some((ListingEvent(json), (host, last)));
                }
                changed.await;
            }
        },
    ))
}
