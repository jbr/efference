//! A session still being prepared, as the TUI shows it (docs/tui.md): the `starting`
//! topic's stage leads the status line, a failure puts the turns it hands back in the
//! composer and keeps the view open after the stream ends, and the strip offers the
//! retry.

use super::*;
use crate::protocol::state::{Stage, StartingView};

/// The status line's lead while the session is starting: what preparation is doing, or
/// why it stopped.
pub(crate) fn line(starting: &StartingView) -> String {
    match (&starting.failure, &starting.authorizing) {
        (Some(failure), _) => format!("did not start: {}", failure.reason),
        (None, Some(server)) => {
            format!("starting — waiting for `{server}` to be authorized in a browser on the host")
        }
        (None, None) if starting.stage == Stage::Environment => {
            "starting — running the env hook".to_string()
        }
        (None, None) => "starting — loading the config and mounting MCP servers".to_string(),
    }
}

impl InteractiveState<'_> {
    /// Fold the `starting` topic. A failure's turns go back in the composer once, and
    /// the status line says what happened and where the retry is.
    pub(super) fn apply_starting(&mut self, starting: Option<StartingView>) {
        let newly_failed = self
            .topics
            .starting
            .as_ref()
            .is_none_or(|s| s.failure.is_none());
        if newly_failed && let Some(failure) = starting.as_ref().and_then(|s| s.failure.clone()) {
            if !failure.withdrawn.is_empty() {
                self.apply_recall(&failure.withdrawn);
            }
            self.ui.debug_message = "↑ to the controls · ⏎ on retry to try again".into();
        }
        self.topics.starting = starting;
        self.dirty = true;
    }

    /// The preparation failed: the view stays open with its reason after the stream
    /// ends, so the operator can retry.
    pub(crate) fn start_failed(&self) -> bool {
        self.topics
            .starting
            .as_ref()
            .is_some_and(|s| s.failure.is_some())
    }

    /// The strip's `retry` on a failed start: a resume is patched again, and a
    /// creation reopens the new-session form. Either answer switches to the session,
    /// carrying the composer's text there.
    pub(super) fn retry_start(&mut self) {
        let Some(starting) = self.topics.starting.clone() else {
            return;
        };
        if starting.resume {
            self.ui.debug_message = format!("resuming {}…", starting.id).into();
            let _ = self.host_tx.send(HostCmd::Resume(starting.id));
        } else {
            self.ui_mode = UiMode::Sessions(sessions::SessionMenu {
                note: Some("reading the host's connections…".into()),
                ..Default::default()
            });
            let _ = self.host_tx.send(HostCmd::NewSession);
        }
    }

    /// Where a switch made from this view goes: after a failed start, with the
    /// composer's text, so the turns it handed back reach the session the retry made.
    pub(super) fn switch_exit(&self, id: String) -> Exit {
        let draft = self.composer.text();
        if self.start_failed() && !draft.is_empty() {
            Exit::SwitchWithDraft { id, draft }
        } else {
            Exit::Switch { id }
        }
    }
}

#[cfg(test)]
mod test;
