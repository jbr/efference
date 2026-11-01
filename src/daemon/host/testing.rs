//! The tests' wait on a host (docs/control-channel.md): the same wait as a session's
//! ([`Session::wait_until`]), woken by the host's listing signal.

use super::*;

impl Host {
    /// Wait until `ready` holds, reading it again each time the listing signals that a
    /// session arrived or left (`listing.rs`). Every wake of a hosted session's status
    /// watcher pings the listing as well, so a predicate over one session's state is
    /// woken by it too; one that has a session to wait on should use
    /// [`Session::wait_until`] instead.
    pub(crate) async fn wait_until(&self, what: &str, ready: impl FnMut() -> bool) {
        crate::session::testing::until(
            what,
            || vec![self.listing_changed.notified()],
            None,
            ready,
            || format!("sessions: {}", self.listing_json()),
        )
        .await;
    }

    /// [`wait_until`](Self::wait_until) for a predicate that also reads something the host
    /// raises no signal for, such as a directory a session's end removes or the register of
    /// session ids: read again every poll interval as well as at each signal.
    pub(crate) async fn poll_until(&self, what: &str, ready: impl FnMut() -> bool) {
        crate::session::testing::until(
            what,
            || vec![self.listing_changed.notified()],
            Some(crate::session::testing::POLL),
            ready,
            || format!("sessions: {}", self.listing_json()),
        )
        .await;
    }
}
