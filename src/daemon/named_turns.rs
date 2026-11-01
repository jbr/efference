//! A turn that says which question it answers (docs/control-api.md): how an agent's
//! message reaches a session. A pending `ask` takes such a turn as its reply only when
//! the turn names it; a turn naming no question waits in the queue, and is refused
//! while a question is pending, so a message written before the question existed is
//! never read as its answer.

use super::transcript::reference::{Referenced, call_id_at, referenced};
use super::transcript::sender;
use super::*;
use crate::protocol::transcript::Handle;
use crate::protocol::turns::Landing;

/// Why a named turn was not delivered. Each renders as the refusal its sender reads
/// ([`NotDelivered::message`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotDelivered {
    /// A question is pending, and the turn names none.
    Asking {
        question: String,
        handle: Option<String>,
    },
    /// The turn names a question that is no longer pending.
    Stale {
        handle: String,
        referenced: Box<Referenced>,
    },
    /// The turn names something that is not a question this session asked.
    NotAQuestion {
        reference: String,
        referenced: Box<Referenced>,
    },
    /// The turn names a question and carries nothing to answer it with.
    Blank,
}

impl NotDelivered {
    /// The refusal as the sender reads it: what happened, then what would work. `id`
    /// is the target session, `me` the sender as a `source` or an id.
    pub(crate) fn message(&self, id: &str, me: Option<&str>) -> String {
        match self {
            Self::Asking {
                question,
                handle: Some(handle),
            } => format!(
                "not delivered: {id} is asking ({handle}) {question:?} — reply with reply_to: \
                 {handle:?}"
            ),
            Self::Asking {
                question,
                handle: None,
            } => format!(
                "not delivered: {id} is asking {question:?} — reply with reply_to naming the \
                 question's seq from its status"
            ),
            Self::Stale { handle, referenced } => {
                let (question, answer) = match referenced.as_ref() {
                    Referenced::Question {
                        question, answer, ..
                    } => (question.as_str(), answer.as_ref()),
                    _ => ("", None),
                };
                let what = match answer {
                    Some(answer) if answer.is_error => {
                        format!("ended without an answer: {}", answer.text)
                    }
                    Some(answer) => format!(
                        "was already answered by {}: {:?}",
                        sender(answer.source.as_deref(), me),
                        answer.text
                    ),
                    None => format!("is not the question {id} is waiting on"),
                };
                format!(
                    "not delivered: {id}'s question {handle} {question:?} {what}; to send this \
                     as a new message instead, leave out reply_to"
                )
            }
            Self::NotAQuestion {
                reference,
                referenced,
            } => {
                let what = match referenced.as_ref() {
                    Referenced::Call { tool, .. } => format!("is a {tool} call"),
                    _ => format!("names no call in {id}'s transcript"),
                };
                format!(
                    "not delivered: {reference} {what}, not a question; reply_to takes the seq \
                     of a question {id} asked, as its status gives it"
                )
            }
            Self::Blank => {
                "not delivered: a blank reply answers nothing; write the answer as text".into()
            }
        }
    }
}

impl Session {
    /// Submit a turn that names the question it answers by `answers` — a transcript
    /// handle (`3.1`) or the ask's call id — or names none. Answers the pending ask
    /// only when it is the one named; refuses a turn naming no question while one is
    /// pending, and one naming a question that is not pending, so a stale reply is
    /// never delivered as a new message. Decided under the ask lock, so no ask can
    /// start between the check and the queue.
    pub(crate) fn submit_named_turn(
        &self,
        turn: SubmittedTurn,
        answers: Option<&str>,
    ) -> Result<Landing, NotDelivered> {
        let named = answers.map(|reference| (reference, self.call_id_of(reference)));
        if named.is_some() && turn.is_blank() {
            return Err(NotDelivered::Blank);
        }
        let mut ask = self.ask.lock().unwrap();
        match (ask.as_ref(), named) {
            (Some(state), Some((_, Some(id)))) if state.call_id == id => {
                let state = ask.take().expect("matched above");
                drop(ask);
                self.person_acted(PersonAction::NamedTurn { answers: true });
                // An agent's answer is delivered, and the loop then waits at its next
                // juncture if the operator holds it (docs/control-channel.md).
                if !crate::record::names::sources::agent_source(turn.source.as_deref()) {
                    self.resume_from_hold();
                }
                self.deliver_reply(state, turn);
                Ok(Landing::Answered)
            }
            (Some(state), None) => {
                let (question, call_id) = (state.question.clone(), state.call_id.clone());
                drop(ask);
                Err(NotDelivered::Asking {
                    question,
                    handle: self.handle_of(&call_id),
                })
            }
            (_, Some((reference, id))) => {
                drop(ask);
                Err(self.not_pending(reference, id.as_deref()))
            }
            (None, None) => {
                let landing = self.turn_delivery();
                self.person_acted(PersonAction::NamedTurn { answers: false });
                Ok(match self.queue_turn(ask, turn) {
                    true => Landing::Held,
                    false => landing,
                })
            }
        }
    }

    /// The call a reference names: a handle resolved against the log, else the
    /// reference taken as a call id.
    fn call_id_of(&self, reference: &str) -> Option<String> {
        let log = self.log.lock().unwrap();
        match Handle::parse(reference) {
            Ok(handle) => call_id_at(log.entries(), handle),
            Err(_) => Some(reference.to_string()),
        }
    }

    /// The transcript handle of the call `call_id`.
    pub(crate) fn handle_of(&self, call_id: &str) -> Option<String> {
        transcript::call_handle(self.log.lock().unwrap().entries(), call_id)
    }

    /// Why a reference to a question that is not pending is refused.
    fn not_pending(&self, reference: &str, call_id: Option<&str>) -> NotDelivered {
        let referenced = match call_id {
            Some(id) => referenced(self.log.lock().unwrap().entries(), id),
            None => Referenced::Nothing,
        };
        match &referenced {
            Referenced::Question { handle, .. } => NotDelivered::Stale {
                handle: handle.clone(),
                referenced: Box::new(referenced),
            },
            _ => NotDelivered::NotAQuestion {
                reference: reference.to_string(),
                referenced: Box::new(referenced),
            },
        }
    }
}

#[cfg(test)]
mod test;
