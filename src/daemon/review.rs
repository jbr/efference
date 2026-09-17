//! The `upstream` review position and its resolution (docs/permissions.md).
//!
//! [`ReviewLevel::Upstream`] names the *creating session* as the reviewer of a call
//! the spotter held. The link it needs — who created this session, and whether that
//! session is still running — is host control state, so a hosted session carries a
//! weak handle back to its host alongside its creator's id.
//!
//! Resolution is **continuous**: [`Session::resolved_review_level`] is asked at the
//! moment a call is routed, never once at creation. A session whose creator has ended
//! reviews as [`ReviewLevel::SpotterHuman`] from that moment on, in the same fail-safe
//! direction as [`ReviewLevel::from_u8`] — an unresolvable position moves toward the
//! operator, never toward autonomous.

use super::host::Host;
use super::*;

use crate::policy::ReviewLevel;

/// A hosted session's link back to the session that commissioned it: the creator's
/// id, and the host that answers whether it is still running. Held weakly — the host
/// owns every session, so a strong handle would be a cycle.
pub(super) struct Upstream {
    host: std::sync::Weak<Host>,
    creator: String,
}

impl Session {
    /// Record the session that commissioned this one, called by `Host::adopt_with`.
    pub(super) fn set_upstream(&self, host: &Arc<Host>, creator: String) {
        *self.upstream.lock().unwrap() = Some(Upstream {
            host: Arc::downgrade(host),
            creator,
        });
    }

    /// The session that answers this one's gates under [`ReviewLevel::Upstream`], when
    /// there is one: the creator, and only while it is still hosted. `None` for a root
    /// session, for a session built without a host, and for a session whose creator has
    /// ended.
    pub fn upstream(&self) -> Option<String> {
        // Read the link out and drop the guard before touching the host: the host's
        // session map is a lock of its own, and this one is never held across it.
        let (host, creator) = {
            let guard = self.upstream.lock().unwrap();
            let link = guard.as_ref()?;
            (link.host.upgrade()?, link.creator.clone())
        };
        host.session(&creator)?;
        Some(creator)
    }

    /// The review position **in force right now** (docs/permissions.md): the dial's own
    /// value, except that `upstream` with no live upstream reviews as `spotter_human`.
    /// This is what gate routing and `GET /state` read; the raw dial is what a client set
    /// and what it reads back.
    pub fn resolved_review_level(&self) -> ReviewLevel {
        match self.review_level() {
            ReviewLevel::Upstream if self.upstream().is_none() => ReviewLevel::SpotterHuman,
            level => level,
        }
    }
}
