//! What a session is called and what it is for: its identity, title, and
//! successor (docs/session-title.md).

use super::*;

impl Session {
    /// Store what this session is — the model it was configured with and the
    /// directory it runs in (docs/control-api.md). Called once by the runtime before
    /// the loop starts; the `session_identity` topic reads it back.
    pub fn set_identity(&self, identity: SessionIdentity) {
        *self.identity.lock().unwrap() = Some(identity);
        self.touch_state();
    }

    /// What this session is, as the `session_identity` topic reports it; `None`
    /// where no session booted.
    pub fn identity(&self) -> Option<SessionIdentity> {
        self.identity.lock().unwrap().clone()
    }

    /// Record a title decision (docs/session-title.md): append the `Title` entry
    /// and move the identity's `title`, so the topic republishes. `Ok(None)` when
    /// no identity was set (a session read with no daemon behind it), in which
    /// case nothing is appended either.
    pub fn set_title(&self, title: crate::record::log::Title) -> io::Result<Option<Seq>> {
        let mut guard = self.identity.lock().unwrap();
        let Some(identity) = guard.as_mut() else {
            return Ok(None);
        };
        let seq = self.append(self.latest_turn(), Payload::Title(title.clone()))?;
        identity.title = Some(title.text);
        drop(guard);
        self.touch_state();
        Ok(Some(seq))
    }

    /// The session's id and the call held at its gate now: what a transcript reading names
    /// and shows as waiting. Read before the log is locked.
    pub(crate) fn reading_state(&self) -> (Option<String>, Option<String>) {
        let session = self
            .id
            .clone()
            .or_else(|| self.identity().map(|identity| identity.id));
        let held = self
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.gate.call_id.clone());
        (session, held)
    }

    /// Record the session this one handed off to (docs/tools.md), so the
    /// `session_identity` topic republishes and attached clients follow it.
    pub fn set_successor(&self, successor: &str) {
        if let Some(identity) = self.identity.lock().unwrap().as_mut() {
            identity.successor = Some(successor.to_string());
        }
        self.touch_state();
    }
}
