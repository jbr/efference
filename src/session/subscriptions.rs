//! How many other sessions this one is subscribed to (docs/tools.md): the host's
//! subscriber sets live in `host/subscriptions.rs`, and this is the count they
//! mirror here so the `wait` tool can tell that a subscribed session has something
//! that may wake it (docs/process-monitor.md).

use super::*;

impl Session {
    /// Whether this session is subscribed to any other (docs/tools.md) — a source
    /// that may wake it, as the `wait` tool counts them (docs/process-monitor.md).
    pub fn has_subscriptions(&self) -> bool {
        self.subscriptions.load(Ordering::SeqCst) > 0
    }

    pub(crate) fn subscription_added(&self) {
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn subscription_removed(&self) {
        let _ = self
            .subscriptions
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            });
    }
}
