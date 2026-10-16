//! A session's supervisor, set after creation (docs/permissions.md).
//!
//! The pointer is `Hosted::supervisor`, which starts as `created_by`. Setting it to
//! another session is one action carrying everything supervision means: the session's
//! gates under the `supervisor` review position route to the new supervisor, the new
//! supervisor may drive it (the authority rule in docs/tools.md reads this pointer),
//! and it is subscribed to the session's status with wake, as `create` subscribes a
//! creator. Clearing or moving the pointer drops the previous supervisor's
//! subscription; a held gate is re-routed by the status watcher (`subscriptions.rs`),
//! which is level-triggered on who the supervisor is.
//!
//! The pointer is host control state and is not persisted: a resumed session starts
//! again from its creator. Each change is recorded in the supervised session's log as
//! a `Telemetry::SupervisorSet` entry, which projects to nothing.

use super::*;

/// Why a supervisor could not be set.
#[derive(Debug, PartialEq, Eq)]
pub enum SupervisorRefused {
    /// The session being changed is not hosted here.
    NotHosted(String),
    /// The change was refused, with the reason: an unknown supervisor, one in
    /// another project, or a cycle.
    Refused(String),
}

impl Host {
    /// The supervisor pointer for `id` (docs/permissions.md), whether or not the
    /// session it names is still hosted. `None` when `id` is not hosted or has no
    /// supervisor. What the authority rule reads; [`Session::supervisor`] is the
    /// live-resolved value gate routing reads.
    pub fn supervisor_of(&self, id: &str) -> Option<String> {
        self.sessions_mut()
            .hosted(id)
            .and_then(|hosted| hosted.supervisor.clone())
    }

    /// Every hosted session whose supervisor pointer names `id` — the sessions whose
    /// `supervisor` review position resolves through it.
    pub(super) fn supervised_by(&self, id: &str) -> Vec<String> {
        self.sessions_mut()
            .hosted_iter()
            .filter(|(_, hosted)| hosted.supervisor.as_deref() == Some(id))
            .map(|(supervised, _)| supervised.clone())
            .collect()
    }

    /// Set `id`'s supervisor to `supervisor`, or clear it with `None`
    /// (docs/permissions.md). Returns the pointer as it now reads.
    ///
    /// Refused when the new supervisor is not hosted, is in another project (it could
    /// not reach `id` to answer its gates), or would make the supervisor chain cycle —
    /// checked by walking the chain under the same lock that writes the pointer.
    /// Setting the value the pointer already holds changes nothing.
    ///
    /// A set moves a dial sitting at the supervised default (`spotter_human`) to
    /// `supervisor`, as adoption does for a created session, subscribes the new
    /// supervisor with wake, and tells it so in its inbox.
    pub fn set_supervisor(
        &self,
        id: &str,
        supervisor: Option<String>,
    ) -> std::result::Result<Option<String>, SupervisorRefused> {
        let (session, previous) = {
            let mut sessions = self.sessions_mut();
            let Some(hosted) = sessions.hosted(id) else {
                return Err(SupervisorRefused::NotHosted(id.to_string()));
            };
            let session = Arc::clone(&hosted.session);
            if let Some(next) = &supervisor {
                let Some(candidate) = sessions.hosted(next) else {
                    return Err(SupervisorRefused::Refused(format!(
                        "no session {next:?} on this host"
                    )));
                };
                if candidate.session.storage_base() != session.storage_base() {
                    return Err(SupervisorRefused::Refused(format!(
                        "{next:?} is in another project than {id:?}, so it could not reach \
                         {id:?} to answer its gates"
                    )));
                }
                if let Some(cycle) = cycle_through(&sessions, id, next) {
                    return Err(SupervisorRefused::Refused(format!(
                        "{next:?} supervising {id:?} would cycle the supervisor chain: {}",
                        cycle.join(" → ")
                    )));
                }
            }
            let hosted = sessions
                .hosted_mut(id)
                .expect("checked above, under this lock");
            let previous = std::mem::replace(&mut hosted.supervisor, supervisor.clone());
            (session, previous)
        };
        if previous == supervisor {
            return Ok(supervisor);
        }
        if let Some(previous) = &previous {
            self.unsubscribe(id, previous);
        }
        session.record_supervisor(supervisor.clone());
        if let Some(next) = &supervisor {
            session.default_to_supervisor();
            self.subscribe(id, next, true);
            if let Some(inbox) = self.session(next) {
                // No slot: on `session:<id>` this notice and a gate the watcher
                // re-routes here could supersede each other in either order.
                inbox.notify_wake(id.to_string(), supervising_body(id), None);
            }
        }
        // Republishes the `supervisor` topic, and wakes the status watcher so a held
        // gate is re-routed to whoever answers it now.
        session.touch_state();
        Ok(supervisor)
    }
}

/// The chain `id → next → … → id` that pointing `id` at `next` would close, when it
/// would: `next`'s supervisor pointers are followed until they reach `id`, end, or
/// leave the host. A pre-existing loop that does not pass through `id` ends the walk
/// rather than spinning.
fn cycle_through(sessions: &super::Sessions, id: &str, next: &str) -> Option<Vec<String>> {
    let mut chain = vec![id.to_string(), next.to_string()];
    let mut seen = std::collections::HashSet::new();
    let mut current = next.to_string();
    loop {
        if current == id {
            return Some(chain);
        }
        if !seen.insert(current.clone()) {
            return None;
        }
        let up = sessions.hosted(&current)?.supervisor.clone()?;
        chain.push(up.clone());
        current = up;
    }
}

/// What a new supervisor hears when it is given a session to supervise.
fn supervising_body(id: &str) -> String {
    format!(
        "You now supervise {id}: its gates come to you to resolve with sessions__approve, \
         you may drive it with sessions__turn and sessions__interrupt, and its status \
         reaches your inbox at each decision point. sessions__state shows where it is now. \
         sessions__review_level (spotter_human) hands its gates back to the operator."
    )
}

impl Session {
    /// Append a supervisor change to this session's log (docs/permissions.md). A failed
    /// write is reported and dropped: the record must not fail what it describes.
    fn record_supervisor(&self, supervisor: Option<String>) {
        let turn = self.latest_turn();
        let entry =
            crate::log::Payload::Telemetry(crate::log::Telemetry::SupervisorSet { supervisor });
        if let Err(e) = self.append(turn, entry) {
            log::warn!("supervisor record: append failed: {e}");
        }
    }
}
