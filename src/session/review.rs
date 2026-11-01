//! The `supervisor` review position and its resolution (docs/permissions.md).
//!
//! [`ReviewLevel::Supervisor`] names the session's **supervisor** as the reviewer of a
//! call the spotter held. The supervisor is a pointer the host holds per session
//! (`Hosted::supervisor`): it starts as the session's creator and can be set or cleared
//! afterwards (`Host::set_supervisor`, `src/daemon/host/supervise.rs`). A hosted session
//! carries a weak handle back to its host, as [`SupervisorPointers`], so it can read the
//! pointer and ask whether the session it names is still running.
//!
//! Resolution is **continuous**: [`Session::resolved_review_level`] is asked at the
//! moment a call is routed, never once at creation. A session whose supervisor has
//! ended, or whose pointer was cleared, reviews as [`ReviewLevel::SpotterHuman`] from
//! that moment on, in the same fail-safe direction as [`ReviewLevel::from_u8`] — an
//! unresolvable position moves toward the operator, never toward autonomous.

use super::*;

use crate::config::policy::ReviewLevel;

/// What a session's review reads of the host that adopted it (docs/permissions.md): the
/// supervisor pointer the host holds for each session, and whether a session is hosted.
/// `Host` implements it (`src/daemon/host/supervise.rs`).
pub(crate) trait SupervisorPointers: Send + Sync {
    /// The supervisor pointer for `id`, whether or not the session it names is still
    /// hosted. `None` when `id` is not hosted or has no supervisor.
    fn supervisor_of(&self, id: &str) -> Option<String>;

    /// Whether a session `id` is hosted now.
    fn holds(&self, id: &str) -> bool;
}

/// A hosted session's link to the host that holds its supervisor pointer: the host,
/// and this session's id there. Held weakly — the host owns every session, so a
/// strong handle would be a cycle.
pub(super) struct SupervisorLink {
    host: std::sync::Weak<dyn SupervisorPointers>,
    id: String,
}

impl Session {
    /// Link this session to the host that adopted it under `id`, called by
    /// `Host::adopt_with`.
    pub(crate) fn link_supervisor(
        &self,
        host: &Arc<impl SupervisorPointers + 'static>,
        id: String,
    ) {
        let host = Arc::downgrade(host);
        *self.supervisor.lock().unwrap() = Some(SupervisorLink { host, id });
    }

    /// The session that answers this one's gates under [`ReviewLevel::Supervisor`], when
    /// there is one: the host's supervisor pointer for this session, and only while the
    /// session it names is still hosted. `None` for a session with no supervisor (a
    /// root, or one whose pointer was cleared), for a session built without a host, and
    /// for a session whose supervisor has ended.
    pub fn supervisor(&self) -> Option<String> {
        // Read the link out and drop the guard before touching the host: the host's
        // session map is a lock of its own, and this one is never held across it.
        let (host, id) = {
            let guard = self.supervisor.lock().unwrap();
            let link = guard.as_ref()?;
            (link.host.upgrade()?, link.id.clone())
        };
        let supervisor = host.supervisor_of(&id)?;
        host.holds(&supervisor).then_some(supervisor)
    }

    /// Whether the host this session lives in holds a session `id` now — what an
    /// escalated gate's routing asks of the session it went to (`status/view.rs`).
    /// False for a session built without a host.
    pub(super) fn host_holds(&self, id: &str) -> bool {
        let host = {
            let guard = self.supervisor.lock().unwrap();
            guard.as_ref().and_then(|link| link.host.upgrade())
        };
        host.is_some_and(|host| host.holds(id))
    }

    /// Move the dial to `supervisor` when it sits at the supervised default,
    /// `spotter_human`: what a session given a supervisor — at creation, or later —
    /// starts reviewing under. A position set explicitly away from that default
    /// (`autonomous`, `human_only`) is kept.
    pub(crate) fn default_to_supervisor(&self) {
        if self.review_level() == ReviewLevel::SpotterHuman {
            self.set_review_level(ReviewLevel::Supervisor);
        }
    }

    /// The review position **in force right now** (docs/permissions.md): the dial's own
    /// value, except that `supervisor` with no live supervisor reviews as `spotter_human`.
    /// This is what gate routing and `GET /state` read; the raw dial is what a client set
    /// and what it reads back.
    pub fn resolved_review_level(&self) -> ReviewLevel {
        match self.review_level() {
            ReviewLevel::Supervisor if self.supervisor().is_none() => ReviewLevel::SpotterHuman,
            level => level,
        }
    }
}

/// Where a gate is routed (docs/permissions.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Routing {
    /// By the dial: the resolved review position and the live supervisor.
    Dial(ReviewLevel, Option<String>),
    /// By the held call's escalations: how many there have been, and the session the
    /// last went to while it is hosted (`None`: the operator). The dial is not read, so
    /// turning it does not route the call back. The live supervisor is still read: it
    /// may decide the call, and a moved pointer changes who reads it that way.
    Escalated {
        hops: usize,
        reviewer: Option<String>,
        supervisor: Option<String>,
    },
}

impl Session {
    /// Where this session's gate is routed now, as the status watcher compares it.
    pub(crate) fn gate_routing(&self) -> Routing {
        let escalations = self
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .map_or_default(|pending| pending.gate.escalations.clone());
        match escalations.is_empty() {
            true => Routing::Dial(self.resolved_review_level(), self.supervisor()),
            false => Routing::Escalated {
                hops: escalations.len(),
                reviewer: self.gate_reviewer(&escalations),
                supervisor: self.supervisor(),
            },
        }
    }
}
