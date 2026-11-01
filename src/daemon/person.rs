//! A person acted on a session's turn (docs/control-api.md): the one entry point
//! the routes call, which feeds both mechanisms waiting on a person — the cold
//! check's park ([`Session::release_cold`]) and a held push
//! ([`Session::cancel_delayed_push`], docs/web-push.md). A new kind of action is
//! taught here what it releases and what it cancels.

use super::*;
use crate::record::log::{PushCancel, ReleasedBy};

/// What a person did to a session's turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonAction {
    /// A turn submitted over `POST /turns` — an answer when an `ask` is pending.
    Turn,
    /// A turn that names the question it answers, or names none (`named_turns.rs`).
    /// Its caller holds the ask lock, so whether it answers is known rather than read.
    NamedTurn { answers: bool },
    /// A decision at an approval gate. `abort` makes no further request, so it
    /// releases no park.
    Decision { aborted: bool },
    /// `POST /loop/continue`.
    Continue,
}

impl Session {
    /// A turn a client submitted (`POST /turns`): the person's action,
    /// then the submission. The harness's own submissions — the kickoff, a hand-off
    /// brief — go to [`Session::submit_turn`] directly.
    pub(crate) fn submit_client_turn(&self, turn: SubmittedTurn) {
        self.person_acted(PersonAction::Turn);
        self.submit_turn(turn);
    }

    /// Note a person's action: release the cold check where the action lets the turn
    /// go on, and cancel the push for the moment it answers.
    pub(crate) fn person_acted(&self, action: PersonAction) {
        let (released_by, cancel) = match action {
            PersonAction::Turn => (
                Some(self.turn_released_by()),
                Some(PushCancel::TurnSubmitted),
            ),
            PersonAction::NamedTurn { answers } => (
                Some(match answers {
                    true => ReleasedBy::Ask,
                    false => ReleasedBy::Turn,
                }),
                Some(PushCancel::TurnSubmitted),
            ),
            PersonAction::Decision { aborted } => (
                (!aborted).then_some(ReleasedBy::Approval),
                Some(PushCancel::GateDecided),
            ),
            PersonAction::Continue => (Some(ReleasedBy::Continue), None),
        };
        if let Some(by) = released_by {
            self.release_cold(by);
        }
        if let Some(cause) = cancel {
            self.cancel_delayed_push(cause);
        }
    }
}
