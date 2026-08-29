//! Whether anyone is watching the session (docs/web-push.md).
//!
//! Watched = the live `GET /events` connection count is nonzero. The transport's
//! lifetime carries the fact, so there is no presence report to send, no connection
//! id to name, and no per-connection visibility state to keep in sync: a client that
//! is not being looked at drops its stream (the web client does this on
//! `visibilitychange`), and a client whose connection dies for any other reason stops
//! counting at the FIN or within the keepalive interval.
//!
//! The count is named state on [`Shared`] rather than an inline check in the send
//! path, so a second consumer reads the same value instead of re-deriving it. Push
//! suppression (`push.rs`) is the only reader today.

use super::*;

/// Whether a stream connection counts as someone watching — `?presence=off` on
/// `GET /events` (docs/control-api.md).
///
/// Static and open-time only: an observer agent or a logging pipe does not become a
/// human mid-stream, so there is no mutation to carry and nothing to reconcile if a
/// connection dies holding it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Presence {
    /// The connection counts — the default, and what every human client gets.
    #[default]
    On,
    /// The connection is excluded from the count.
    Off,
}

impl Presence {
    /// Parse a `?presence=` value, or say what was wrong with it.
    ///
    /// Same rule as `?include=` ([`Include::parse`](super::Include::parse)): an
    /// unspelled value is an error rather than a silent default. A client that meant
    /// `off` and typed `false` would otherwise be counted as a watcher and suppress
    /// the push it was connected to observe.
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("on") => Ok(Self::On),
            Some("off") => Ok(Self::Off),
            Some(other) => Err(format!("presence must be on or off, got {other:?}")),
        }
    }
}

/// One connection's claim on the watched count, released by `Drop`.
///
/// The claim is the connection: it is taken when the stream is built and released
/// when the stream is dropped, whichever way the connection ended. Nothing else
/// decrements, so the count cannot drift and no saturating arithmetic is needed to
/// hide a drift that a bug would otherwise make visible.
///
/// A `?presence=off` connection holds an inert guard (no `Arc`, nothing to release)
/// rather than no guard at all, so the caller has one shape to store.
pub(crate) struct Watching(Option<Arc<Shared>>);

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(shared) = &self.0
            && shared.watchers.fetch_sub(1, Ordering::Relaxed) == 1
        {
            // This reap may be the daemon discovering, at write time, a
            // connection the client closed before the last suppressed event
            // fired — in which case nobody saw it (docs/web-push.md).
            shared.replay_suppressed();
        }
    }
}

impl Shared {
    /// Count this connection as a watcher for as long as the returned guard lives.
    ///
    /// `Relaxed` throughout: the count is the only thing being communicated — no
    /// other memory is published through it — and a reader that observes a
    /// microsecond-stale count sends (or skips) one notification either way, which is
    /// the same outcome the linger window and the receiver-side skip already cover.
    pub(crate) fn watch(self: &Arc<Self>, presence: Presence) -> Watching {
        match presence {
            Presence::On => {
                self.watchers.fetch_add(1, Ordering::Relaxed);
                Watching(Some(Arc::clone(self)))
            }
            Presence::Off => Watching(None),
        }
    }

    /// Whether anyone is watching — a live stream connection exists that did not opt
    /// out. Read by [`Shared::push`](Shared::push) to suppress a notification the
    /// operator is already looking at.
    pub(crate) fn watched(&self) -> bool {
        self.watchers.load(Ordering::Relaxed) > 0
    }

    /// The count itself, for tests that assert the guard's arithmetic rather than the
    /// predicate it feeds.
    #[cfg(test)]
    pub(crate) fn watcher_count(&self) -> usize {
        self.watchers.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod test;
