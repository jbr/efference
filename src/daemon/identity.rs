//! What a session is called and what it is for: its identity, title, and
//! successor (docs/session-title.md).

use super::*;

/// What a session is, for a client that has to name it on screen: the model it
/// was configured with and the directory it runs in (docs/control-api.md).
///
/// Both are fixed at boot today. They are carried as a state topic rather than a
/// one-shot read so a client learns them from the connect snapshot along with
/// everything else, and so either moving later reaches clients through the
/// mechanism that already delivers whole-topic replacements.
#[derive(Clone, Debug, Serialize)]
pub struct SessionIdentity {
    /// The session id (docs/session-title.md): its directory name under
    /// `.efference/`, the `--name` a client resolves, the address a peer
    /// messages. Fixed at creation.
    pub id: String,
    /// The current title — the last `Title` entry's text (docs/session-title.md),
    /// `None` before the first is written. The one identity field that moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The native model id boot resolved — the same string the system prompt's
    /// identity line states (docs/context.md).
    pub model: String,
    /// The session's working directory, as the daemon knows it: what the tool
    /// layer was built with, rendered as a path string.
    pub cwd: String,
    /// The host connection and native model behind each role, with whether
    /// triage shares the main client. `None` only for a session built without
    /// boot (tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference: Option<crate::inference::InferenceIdentity>,
    /// The session this one handed its work to with `hand_off` (docs/tools.md),
    /// once it has. An attached client follows it there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
}

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
    pub fn set_title(&self, title: crate::log::Title) -> io::Result<Option<Seq>> {
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

    /// Record the session this one handed off to (docs/tools.md), so the
    /// `session_identity` topic republishes and attached clients follow it.
    pub fn set_successor(&self, successor: &str) {
        if let Some(identity) = self.identity.lock().unwrap().as_mut() {
            identity.successor = Some(successor.to_string());
        }
        self.touch_state();
    }
}
