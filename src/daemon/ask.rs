//! The pending `ask` (docs/supervisor.md): a question the loop has put to the operator
//! and is blocked on. The next submitted turn resolves it instead of opening a user turn.

use super::*;

/// The `ask` awaiting a reply. The sender is consumed by the turn that resolves it.
pub(super) struct AskState {
    call_id: String,
    question: String,
    /// This question's place among the asks of one completion, 1-based, and how many
    /// there are: they are put one at a time.
    n: usize,
    of: usize,
    /// Turns that were queued when the ask started, withdrawn the way
    /// `DELETE /turns/:submission_id` withdraws them.
    recalled: Vec<SubmittedTurn>,
    reply: oneshot::Sender<SubmittedTurn>,
}

/// What `GET /state` and a status body say about the pending ask.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AskView {
    pub call_id: String,
    pub question: String,
    pub n: usize,
    pub of: usize,
    /// The turns recalled when this ask started, in the shape the recall route returns
    /// ([`SubmittedTurn::pending_json`]), for the submitting client to put back in its
    /// composer.
    pub recalled: Vec<Value>,
}

/// The loop's claim on a pending ask. Dropping it ends the ask — on a reply, an
/// interrupt, or an aborted batch — so nothing stays pending afterwards.
pub(crate) struct AskWait<'a> {
    session: &'a Session,
    call_id: String,
    rx: oneshot::Receiver<SubmittedTurn>,
}

impl AskWait<'_> {
    /// The reply, or `None` if the ask was ended from elsewhere.
    pub(crate) async fn reply(&mut self) -> Option<SubmittedTurn> {
        (&mut self.rx).await.ok()
    }
}

impl Drop for AskWait<'_> {
    fn drop(&mut self) {
        {
            let mut ask = self.session.ask.lock().unwrap();
            if ask.as_ref().is_some_and(|a| a.call_id == self.call_id) {
                *ask = None;
            }
        }
        // A reply that landed as the ask ended (an interrupt won the race) is not lost:
        // it opens the next turn, as a submission with nothing pending would.
        if let Ok(turn) = self.rx.try_recv() {
            self.session.submit_turn(turn);
        }
        self.session.settle_changed.notify_waiters();
        self.session.touch_state();
    }
}

impl Session {
    /// Start an ask and recall the turns queued before it (docs/supervisor.md): they
    /// were written before the question existed, so they are neither its answer nor
    /// left behind it. Nothing is logged; the recalled turns are published on the ask
    /// ([`Session::asking`]).
    pub(crate) fn begin_ask(
        &self,
        call_id: &str,
        question: String,
        n: usize,
        of: usize,
    ) -> AskWait<'_> {
        let (tx, rx) = oneshot::channel();
        let question_for_push = question.clone();
        {
            let mut ask = self.ask.lock().unwrap();
            let recalled: Vec<SubmittedTurn> = self.queue.lock().unwrap().drain(..).collect();
            *ask = Some(AskState {
                call_id: call_id.to_string(),
                question,
                n,
                of,
                recalled,
                reply: tx,
            });
        }
        self.settle_changed.notify_waiters();
        self.touch_state();
        // The session is waiting on a person, as it is when a turn ends
        // (docs/web-push.md); the message is the question.
        self.push("Message", super::push::ask_body(&question_for_push));
        AskWait {
            session: self,
            call_id: call_id.to_string(),
            rx,
        }
    }

    /// The pending ask, if any.
    pub(crate) fn asking(&self) -> Option<AskView> {
        self.ask.lock().unwrap().as_ref().map(|a| AskView {
            call_id: a.call_id.clone(),
            question: a.question.clone(),
            n: a.n,
            of: a.of,
            recalled: a.recalled.iter().map(SubmittedTurn::pending_json).collect(),
        })
    }

    /// Hand a submitted turn to the ask it resolves.
    pub(super) fn deliver_reply(&self, state: AskState, turn: SubmittedTurn) {
        // The receiver is gone only if the loop dropped its claim, which clears the ask
        // first; a failed send returns the turn to the queue path rather than losing it.
        if let Err(turn) = state.reply.send(turn) {
            self.submit_turn(turn);
            return;
        }
        self.settle_changed.notify_waiters();
        self.touch_state();
    }
}
