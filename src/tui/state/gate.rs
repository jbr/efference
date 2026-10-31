//! Approval-gate handling: the composer's submit at a gate and the strip's gate verbs (docs/tui.md).

use super::*;

impl<'a> InteractiveState<'a> {
    /// The cold line while the session is believed cold (docs/tui.md): what the next
    /// action reads uncached — an approval at a gate, an answer to an `ask`, or a send
    /// from the composer. The status bar draws it for as long as this is `Some`. It is
    /// information: the action goes through either way.
    pub(in crate::tui) fn cold_line(&self) -> Option<String> {
        let cold = self.cold?;
        let subject = if self.pending_gate.is_some() {
            "approving or redirecting"
        } else if self.ask.is_some() {
            "your answer"
        } else {
            "a send"
        };
        let cost = match cold.context_tokens {
            Some(tokens) => format!("~{}k tokens uncached", (tokens + 500) / 1_000),
            None => "the context uncached".to_string(),
        };
        Some(format!("cache expired — {subject} reads {cost}"))
    }

    /// Handle a composer submission. When a gate is pending, the text resolves it:
    /// empty approves, anything else redirects the model with that text (docs/tui.md —
    /// the composer composes text only; abort is the strip's verb). Otherwise it's a
    /// normal user turn (`POST /turns`).
    pub(in crate::tui) fn submit_composer(&mut self, text: String) {
        if let Some(gate) = self.pending_gate.take() {
            let trimmed = text.trim();
            let (decision, label) = if trimmed.is_empty() {
                (Approval::Approve, "approved")
            } else {
                (Approval::Redirect(trimmed.to_string()), "redirected")
            };
            let _ = self.approve_tx.send((gate.call_id, decision));
            self.composer.clear();
            self.ui.debug_message = label.into();
            self.viewport.stuck_to_bottom = true;
            return;
        }

        // A send carries its staged files, so it waits for them: a chip still uploading has no
        // name to send yet.
        if self.staged.uploading() {
            self.ui.debug_message = "an attachment is still uploading".into();
            self.dirty = true;
            return;
        }
        // Mint the submission id here (docs/tui.md): the echo below and any later recall both
        // address this exact submission, which text equality could not do once a recalled turn
        // can be edited and re-sent.
        let submitted = SubmittedTurn::new(text, crate::daemon::mint_submission_id())
            .with_attachments(self.staged.ready());
        // A send while a question shows answers that question, by name (docs/tui.md).
        let submission = crate::tui::senders::Submission {
            turn: submitted.clone(),
            answers: self.ask.as_ref().map(|ask| ask.call_id.clone()),
        };
        if self.submit_tx.send(submission).is_ok() {
            self.clear_staged();
            // Echo the message immediately as a pending transcript unit
            // (docs/tui.md): it's handed to the daemon but not yet a
            // committed `User` entry, so it renders dimmed until its log entry streams back
            // via `/events`. Reproject now so the echo appears on this keypress rather than
            // waiting for the next daemon event.
            self.pending_turns.push(submitted);
            self.composer.clear();
            self.ui.debug_message = "sent".into();
            // Follow the new turn as it streams in.
            self.viewport.stuck_to_bottom = true;
            self.mark_reproject();
        } else {
            self.ui.debug_message = "could not send (daemon gone?)".into();
        }
    }

    /// Hand control back with no input (docs/tui.md — the empty-composer `⏎`):
    /// fire `POST /loop/continue`, which re-enters the trajectory with nothing
    /// appended. Nothing is echoed as a pending turn, because nothing was said —
    /// the only local effect is the status line and following the tail again.
    /// A daemon decline arrives later as a [`TuiEvent::Notice`].
    pub(in crate::tui) fn continue_trajectory(&mut self) {
        if self.continue_tx.send(()).is_ok() {
            self.ui.debug_message = "continuing".into();
            self.viewport.stuck_to_bottom = true;
        } else {
            self.ui.debug_message = "could not send (daemon gone?)".into();
        }
        self.dirty = true;
    }

    /// Ask for a compaction (backlog: cold-session.md — `C-o`): the session's own model
    /// replaces its history with an account instead of the cold read.
    /// The daemon's answer comes back as a [`TuiEvent::Notice`].
    pub(in crate::tui) fn compact_session(&mut self) {
        self.ui.debug_message = if self.compaction_tx.send(()).is_ok() {
            "asking for a compaction".into()
        } else {
            "could not send (daemon gone?)".into()
        };
        self.dirty = true;
    }

    /// Pull the most recent queued-but-unsent turn back into the composer (docs/tui.md — the
    /// recall gesture). Nothing has been sent to the model yet: a pending turn sits in the
    /// daemon's queue, unlogged and unprojected, so this is ordinary ephemeral control state.
    ///
    /// The request is a **suffix pop** addressed by submission id — asking for an older turn
    /// takes it and everything after it — and the composer is spliced only when the daemon
    /// answers with what it actually popped ([`apply_recall`](Self::apply_recall)).
    pub(in crate::tui) fn recall_pending_turn(&mut self) {
        let Some(last) = self.pending_turns.last() else {
            self.ui.debug_message = "nothing queued to recall".into();
            self.dirty = true;
            return;
        };
        if self.recall_tx.send(last.submission_id.clone()).is_err() {
            self.ui.debug_message = "could not send (daemon gone?)".into();
        }
        self.dirty = true;
    }

    /// Splice the turns a recall actually withdrew into the composer, in submission order, ahead
    /// of whatever draft is already there — joined with a newline, since adjacent pending turns
    /// are separate blocks on the wire and popping several into one buffer has to invent a join.
    /// The buffer is the uncommitted tail, so nothing here touches the log or the cached prefix.
    ///
    /// An empty list is the lost race: the loop dequeued the turn between the keypress and the
    /// withdraw. Nothing is spliced and the status line says so — the echo stays until its `User`
    /// entry settles it.
    pub(in crate::tui) fn apply_recall(&mut self, withdrawn: &[crate::client::WithdrawnTurn]) {
        if withdrawn.is_empty() {
            self.ui.debug_message = "already sent — nothing recalled".into();
            self.dirty = true;
            return;
        }
        for turn in withdrawn {
            self.pending_turns
                .retain(|p| p.submission_id != turn.submission_id);
        }
        // Their attachments are still in the store: staged again, ready to re-send.
        self.staged.restore(
            withdrawn
                .iter()
                .flat_map(|t| t.attachments.iter().cloned())
                .collect(),
        );
        let mut buffer = withdrawn
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let draft = self.composer.text().to_string();
        if !draft.is_empty() {
            buffer.push('\n');
            buffer.push_str(&draft);
        }
        self.composer.set_text(&buffer);
        self.ui.debug_message = if withdrawn.len() == 1 {
            "recalled".into()
        } else {
            format!("recalled {}", withdrawn.len()).into()
        };
        self.mark_reproject();
    }

    /// A send the daemon refused because it did not name the pending question, or named
    /// one no longer pending (docs/tui.md). Its text comes back to the composer as a recall
    /// would bring it; the question itself arrives on the `ask` topic.
    pub(in crate::tui) fn apply_not_delivered(
        &mut self,
        turn: crate::client::WithdrawnTurn,
        answered: bool,
    ) {
        self.apply_recall(&[turn]);
        self.ui.debug_message = if answered {
            "not sent: that question is no longer waiting; your message is back in the composer"
        } else {
            "not sent: a question came first; your message is back in the composer"
        }
        .into();
    }

    /// Take back the turns an `ask` withdrew from the queue, once per ask (docs/tui.md). Only
    /// those this client submitted come back — a submission id in `pending_turns` — so a
    /// restarted client, or another client's queue, restores nothing. What comes back goes
    /// through [`apply_recall`](Self::apply_recall), as a recall the operator asked for would.
    pub(in crate::tui) fn apply_ask(&mut self, ask: Option<crate::tui::parse::PendingAsk>) {
        // The mark on the question follows the topic, with no log entry behind it.
        if self.ask.as_ref().map(|a| &a.call_id) != ask.as_ref().map(|a| &a.call_id) {
            self.mark_reproject();
        }
        self.ask.clone_from(&ask);
        let Some(ask) = ask else {
            return;
        };
        if self.ask_restored.as_deref() == Some(ask.call_id.as_str()) {
            return;
        }
        self.ask_restored = Some(ask.call_id);
        let own: Vec<_> = ask
            .recalled
            .into_iter()
            .filter(|t| {
                self.pending_turns
                    .iter()
                    .any(|p| p.submission_id == t.submission_id)
            })
            .collect();
        if !own.is_empty() {
            self.apply_recall(&own);
        }
    }

    /// Approve the pending gate without touching the composer — the control
    /// strip's `approve` verb (docs/tui.md). Unlike an empty-composer `⏎` submit,
    /// this leaves any drafted text in place (the draft may be a redirect the
    /// operator decided against sending).
    pub(in crate::tui) fn approve_gate(&mut self) {
        if let Some(gate) = self.pending_gate.take() {
            let _ = self.approve_tx.send((gate.call_id, Approval::Approve));
            self.ui.debug_message = "approved".into();
            self.viewport.stuck_to_bottom = true;
        }
    }

    /// Abort the pending gate — the strip's `abort` verb. Like
    /// `approve_gate`, it leaves any drafted text in place.
    pub(in crate::tui) fn abort_gate(&mut self) {
        if let Some(gate) = self.pending_gate.take() {
            let _ = self.approve_tx.send((gate.call_id, Approval::Abort));
            self.ui.debug_message = "aborted".into();
            self.viewport.stuck_to_bottom = true;
        }
    }
}
