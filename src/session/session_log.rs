//! A session's log as the daemon reads and writes it: appends, projections, and
//! lookups by seq (docs/log-and-projection.md).

use super::*;

/// How far one [`append_committed_locked`](Session::append_committed_locked) batch got
/// (docs/permissions.md). A batch is not transactional: a log write that fails partway leaves
/// the edits before it appended and folded, and the ones from the failure on in neither.
pub(crate) struct CommittedBatch {
    /// The session-visible notices the edits that landed earned, raised by the caller after it
    /// releases the policy section.
    pub(crate) notices: Vec<(String, String)>,
    /// How many of the batch's edits landed, in a prefix of its order.
    pub(crate) landed: usize,
    /// The append error that stopped the batch; `None` when all of them landed.
    pub(crate) error: Option<io::Error>,
}

impl Session {
    /// Run `read` over the log's entries with its lock held, for a readout that renders in
    /// place rather than copying the log out. The lock is only ever held across synchronous
    /// work, never an `await`, so `read` is plain code too.
    pub(crate) fn with_entries<R>(&self, read: impl FnOnce(&[LogEntry]) -> R) -> R {
        read(self.log.lock().unwrap().entries())
    }

    /// Pinged on each log append; the `GET /events` tail and the status watcher wake on it.
    /// Arm a `notified()` before reading the log, so an append between the read and the
    /// await is not lost.
    pub(crate) fn appended(&self) -> &Notify {
        &self.appended
    }

    /// Resolve `$TMPDIR` in a structured call's path fields against the frozen
    /// session env map ([`crate::tools::expand`]) — the loop's one expansion choke
    /// point, upstream of classify/spot/exec. `Ok(None)` = untouched.
    pub fn expand_call(&self, call: &ToolCall) -> Result<Option<ToolCall>, String> {
        crate::tools::expand::expand_call(&self.expander, call)
    }

    /// Append a user turn the loop writes on someone's behalf, a redirect's message,
    /// under its `source`, with no submission behind it (docs/permissions.md).
    pub fn append_sourced(
        &self,
        turn: Turn,
        payload: Payload,
        source: Option<String>,
    ) -> io::Result<Seq> {
        self.append_user(turn, payload, None, source)
    }

    /// [`append`](Self::append) carrying the submission id of the
    /// `POST /v1/turns` the entry realizes (docs/control-api.md) — the loop's
    /// user-turn appends route here so the entry names its originating
    /// submission.
    pub fn append_submission(
        &self,
        turn: Turn,
        payload: Payload,
        submission_id: String,
        source: Option<String>,
    ) -> io::Result<Seq> {
        self.append_user(turn, payload, Some(submission_id), source)
    }

    fn append_user(
        &self,
        turn: Turn,
        payload: Payload,
        submission_id: Option<String>,
        source: Option<String>,
    ) -> io::Result<Seq> {
        // User turns only — a submitted turn is never a `ConfigEdit`, so none of
        // `append`'s proposal-lifecycle handling applies to the entry itself.
        //
        // A turn carrying a filesystem reference is admitted through the grant
        // transaction (`session/grants.rs`), which is what makes the grant exist only
        // once the message is in the log (docs/permissions.md). One carrying none is
        // exactly the append it always was, so no policy lock and no config file are
        // touched at all.
        match grants::filesystem_document(&payload) {
            Some(document) => self.admit(turn, payload, submission_id, source, &document),
            None => self.append_log(turn, payload, submission_id, source),
        }
    }

    /// The log append itself, under the log lock, with no policy work of any kind. Every
    /// entry — a user turn, a config edit, an ambient notification — lands through here.
    pub(super) fn append_log(
        &self,
        turn: Turn,
        payload: Payload,
        submission_id: Option<String>,
        source: Option<String>,
    ) -> io::Result<Seq> {
        #[cfg(test)]
        if let Some(message) = self.fail_log_append.lock().unwrap().take() {
            return Err(io::Error::other(message));
        }
        let seq = {
            let mut log = self.log.lock().unwrap();
            let seq = log.append_submission(turn, payload, submission_id, source)?;
            self.observe_operations(&log);
            seq
        };
        self.appended.notify_waiters();
        Ok(seq)
    }

    /// End the session's log at teardown: release its writer lock so the session can
    /// be resumed at once, and wake SSE tailers so their streams end once they have
    /// delivered every entry. Readers still holding the session keep its entries.
    pub(crate) fn close_log(&self) {
        if let Err(e) = self.log.lock().unwrap().close() {
            log::warn!("log: flushing at close failed: {e}");
        }
        self.appended.notify_waiters();
    }

    /// Whether the log holds no operator or agent turn (`crate::record::log::is_empty`).
    pub(crate) fn log_is_empty(&self) -> bool {
        crate::record::log::is_empty(self.log.lock().unwrap().entries())
    }

    pub(crate) fn log_closed(&self) -> bool {
        self.log.lock().unwrap().is_closed()
    }

    /// Record a deliberate archive: `Archived` under the turn of the log's last entry,
    /// so the turn counter a later resume continues from does not move
    /// (docs/resumption.md). `source` is who archived it (docs/log-and-projection.md).
    pub(crate) fn append_archived(&self, source: Option<String>) -> io::Result<Seq> {
        let turn = self
            .log
            .lock()
            .unwrap()
            .entries()
            .last()
            .map_or(0, |entry| entry.turn);
        self.append(turn, Payload::archived(source))
    }

    /// Append to the log (durably) and wake SSE tailers.
    ///
    /// A `ConfigEdit` also drives the proposal lifecycle (docs/permissions.md): its
    /// `(state, change)` is captured before the payload moves into the log, and acted on only
    /// after a successful append, so a failed write never edits state. A **committed** edit
    /// takes the policy section, so its append and its fold cannot interleave with another
    /// writer's — the log order and the fold order are one order for every committed edit.
    pub fn append(&self, turn: Turn, payload: Payload) -> io::Result<Seq> {
        let edit = match &payload {
            Payload::ConfigEdit(edit) => edit.clone(),
            _ => return self.append_log(turn, payload, None, None),
        };
        match edit.state {
            // A committed edit takes effect: folded into the effective policy so a gate reads
            // current policy in O(1) (no log re-scan).
            EditState::Committed => {
                let (seq, notice) = {
                    let _section = self.policy_section();
                    self.append_committed_one(turn, &edit)?
                };
                // Session-visible edits (`scope`, `paths`) surface as an `Ambient` notification
                // so the session learns its perimeter moved — an optional tail append that never
                // re-renders the cached preamble (docs/permissions.md). Raised after the section
                // is released, because `notify` re-enters `append` for a slot-less
                // notification. `commands`/`tools` notify nothing — their effect reaches the
                // model only through the classifier.
                if let Some((body, slot)) = notice {
                    self.notify(Notification::new("config", body, Some(slot)));
                }
                Ok(seq)
            }
            // A proposal is inert in the effective config: it joins the pending-proposals queue,
            // addressed by its own `seq`, awaiting an orchestrator's commit/cancel.
            EditState::Proposed => {
                let seq = self.append_log(turn, payload, None, None)?;
                self.proposals.lock().unwrap().push(PendingProposal {
                    seq,
                    change: edit.change,
                });
                Ok(seq)
            }
            // A cancel drops the referenced proposal from the queue; the effective config
            // never moved.
            EditState::Cancelled => {
                let seq = self.append_log(turn, payload, None, None)?;
                if let Some(proposal) = edit.resolves {
                    self.resolve_proposal(proposal);
                }
                Ok(seq)
            }
        }
    }

    /// Append one **committed** `ConfigEdit` and fold it into the effective policy, returning
    /// the session-visible notice it earns for the caller to raise after releasing the section
    /// (docs/permissions.md). The caller holds [`policy_section`](Self::policy_section).
    ///
    /// This is the one place a committed edit is appended and folded, so no other writer can
    /// interleave an append between them: the fold order equals the log order by construction
    /// rather than by every caller being careful.
    pub(crate) fn append_committed_one(
        &self,
        turn: Turn,
        edit: &crate::record::log::ConfigEdit,
    ) -> io::Result<(Seq, Option<(String, String)>)> {
        debug_assert_eq!(
            edit.state,
            EditState::Committed,
            "a committed append takes a committed edit"
        );
        // A test's stand-in for a log write that fails partway through a batch: let this many
        // land, then fail without appending (`fail_committed_append_after`).
        #[cfg(test)]
        {
            let mut after = self.fail_committed_append_after.lock().unwrap();
            if let Some(remaining) = after.as_mut() {
                if *remaining == 0 {
                    *after = None;
                    return Err(io::Error::other("test: the committed append failed"));
                }
                *remaining -= 1;
            }
        }
        let seq = self.append_log(turn, Payload::ConfigEdit(edit.clone()), None, None)?;
        // Where a test lands a competing writer between the append and the fold
        // (`between_append_and_fold`); the policy section must still be held here.
        #[cfg(test)]
        {
            let interleaved = self.between_append_and_fold.lock().unwrap().take();
            if let Some(interleaved) = interleaved {
                interleaved(self);
            }
        }
        {
            let mut eff = self.effective.lock().unwrap();
            let mut role = (**eff).clone();
            crate::config::policy::apply_config_change(&mut role, &edit.change);
            *eff = Arc::new(role);
        }
        // Resolving a proposal: drop the referenced proposal from the pending queue. `None` for a
        // direct synchronous/fs-watch commit, which had no queued proposal.
        if let Some(proposal) = edit.resolves {
            self.resolve_proposal(proposal);
        }
        Ok((seq, session_visible_notice(&edit.change)))
    }

    /// Append `edits` in order, folding each as it lands, and report how far it got
    /// ([`CommittedBatch`]) for the caller to raise and describe after releasing the policy
    /// section (docs/permissions.md). The caller holds [`policy_section`](Self::policy_section)
    /// — a batch is one admission's grants, or the deltas one refresh of the config file owes
    /// the log.
    ///
    /// A failure partway through is not rolled back: the edits before it are in the log and
    /// folded, and the ones from it on are in neither. Nothing retries.
    pub(crate) fn append_committed_locked(
        &self,
        turn: Turn,
        edits: &[crate::record::log::ConfigEdit],
    ) -> CommittedBatch {
        let mut notices = Vec::new();
        for (landed, edit) in edits.iter().enumerate() {
            match self.append_committed_one(turn, edit) {
                Ok((_seq, notice)) => notices.extend(notice),
                Err(error) => {
                    return CommittedBatch {
                        notices,
                        landed,
                        error: Some(error),
                    };
                }
            }
        }
        CommittedBatch {
            notices,
            landed: edits.len(),
            error: None,
        }
    }

    /// Mask `payload` exactly as an append would, writing nothing (docs/env.md). The
    /// admission's probe uses this before it believes a grant the log would rewrite —
    /// whatever the masker does, it does here first (docs/permissions.md).
    pub(crate) fn mask_payload(&self, payload: &mut Payload) {
        self.log.lock().unwrap().mask_payload(payload);
    }

    /// Publish an ephemeral pre-commit output delta to the control stream's `output`
    /// subscribers (docs/control-api.md). Intentionally lossy: a send with no
    /// subscribers — or to a lagging one — is dropped, because the committed entry on
    /// the `log` class is the authoritative copy. Never touches the log.
    pub fn publish_delta(&self, delta: StreamDelta) {
        let _ = self.stream_tx.send(delta);
    }

    /// A fresh subscription to the delta fan-out for one control-stream connection.
    pub(crate) fn subscribe_stream(&self) -> broadcast::Receiver<StreamDelta> {
        self.stream_tx.subscribe()
    }

    /// Project the current log into the wire (docs/README.md). Lock held only for the
    /// synchronous fold.
    pub fn project_messages(&self) -> Vec<ApiMessage> {
        project_with(self.log.lock().unwrap().entries(), self.image_placement)
    }

    /// The dialect-neutral fold of the current log (docs/log-and-projection.md) —
    /// what a request encodes. Lock held only for the synchronous fold.
    pub fn project_turns(&self) -> Vec<WireTurn> {
        project_semantic(self.log.lock().unwrap().entries())
    }

    /// Adopt a consolidation written over the `frozen` snapshot, unless something
    /// appended since changes what that range projects to (docs/consolidate.md): a
    /// fold, a rollback, a verdict on a result at or before the snapshot's end, or
    /// a `consolidate` call whose own fold was computed without this one. Ordinary
    /// newer work, including calls still awaiting results, stays after the range.
    pub(crate) fn adopt_consolidation(
        &self,
        frozen: &[LogEntry],
        span: crate::curation::consolidate::Span,
        note: String,
    ) -> io::Result<bool> {
        {
            let mut log = self.log.lock().unwrap();
            let entries = log.entries();
            let frozen_end = frozen.last().map(|e| e.seq);
            if entries.len() < frozen.len()
                || entries.get(frozen.len().wrapping_sub(1)).map(|e| e.seq) != frozen_end
                || entries[frozen.len()..].iter().any(|e| match &e.payload {
                    Payload::Summary(_) | Payload::Resumed(_) => true,
                    Payload::ResultVerdict { for_result, .. } => {
                        frozen_end.is_some_and(|end| *for_result <= end)
                    }
                    Payload::Assistant { output, .. } => output.tool_calls().is_ok_and(|calls| {
                        calls
                            .iter()
                            .any(|c| c.name == crate::record::names::tools::CONSOLIDATE)
                    }),
                    _ => false,
                })
            {
                return Ok(false);
            }
            let turn = entries.last().map_or(0, |e| e.turn);
            log.append(
                turn,
                Payload::Summary(crate::record::log::Summary::consolidation(
                    span.from, span.upto, note,
                )),
            )?;
            self.observe_operations(&log);
        }
        self.appended.notify_waiters();
        Ok(true)
    }

    /// A clone of the entries — handed to the triager, which awaits the
    /// model and so can't borrow across the lock (docs/triage.md).
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.log.lock().unwrap().entries().to_vec()
    }

    /// The turn of the most recent log entry (`0` if empty) — the turn an **out-of-band** append
    /// attaches to (the fs-watch initiator, which has no loop-turn context,
    /// docs/permissions.md). A `ConfigEdit` projects to nothing, so its turn
    /// is pure audit grouping; attaching it to the in-flight turn is the honest placement.
    pub(crate) fn latest_turn(&self) -> Turn {
        self.log
            .lock()
            .unwrap()
            .entries()
            .last()
            .map_or(0, |e| e.turn)
    }

    /// A clone of the single entry at `seq` (== its index in the log), or `None`
    /// if the log hasn't reached it yet. The `/events` tail reads one entry at a
    /// time, so it never clones the whole log. The loop reads back the result it
    /// just captured, to hand triage the bytes the log holds (docs/triage.md).
    pub(crate) fn entry_at(&self, seq: u64) -> Option<LogEntry> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .get(seq as usize)
            .cloned()
    }

    /// The calls that ran with the operator's consent, folded under the log lock so
    /// the whole log is never cloned (docs/permissions.md). The decision rung's
    /// precedent evidence.
    pub(crate) fn consented_calls(&self) -> Vec<ToolCall> {
        crate::curation::spotter::precedent::consented(self.log.lock().unwrap().entries())
    }
}
