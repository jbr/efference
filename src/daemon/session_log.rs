//! A session's log as the daemon reads and writes it: appends, projections, and
//! lookups by seq (docs/log-and-projection.md).

use super::*;

impl Session {
    /// Resolve `$TMPDIR` in a structured call's path fields against the frozen
    /// session env map ([`crate::expand`]) — the loop's one expansion choke
    /// point, upstream of classify/spot/exec. `Ok(None)` = untouched.
    pub fn expand_call(
        &self,
        call: &crate::log::ToolCall,
    ) -> Result<Option<crate::log::ToolCall>, String> {
        crate::expand::expand_call(&self.expander, call)
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
        // User turns only — a submitted turn is never a `ConfigEdit`, so none of
        // `append`'s proposal-lifecycle handling applies here.
        let seq = {
            let mut log = self.log.lock().unwrap();
            let seq = log.append_submission(turn, payload, Some(submission_id), source)?;
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

    pub(crate) fn log_closed(&self) -> bool {
        self.log.lock().unwrap().is_closed()
    }

    /// Record a deliberate archive: `Archived` under the turn of the log's last entry,
    /// so the turn counter a later resume continues from does not move
    /// (docs/resumption.md).
    pub(crate) fn append_archived(&self) -> io::Result<Seq> {
        let turn = self
            .log
            .lock()
            .unwrap()
            .entries()
            .last()
            .map_or(0, |entry| entry.turn);
        self.append(turn, Payload::Archived {})
    }

    /// Append to the log (durably) and wake SSE tailers. The single appender is
    /// the loop; handlers only read.
    pub fn append(&self, turn: Turn, payload: Payload) -> io::Result<Seq> {
        // A `ConfigEdit` drives the proposal lifecycle (docs/permissions.md):
        // capture its `(state, change)` before the payload moves into the log, then act on it
        // *after* a successful append, so a failed write never edits state. Cheap clone, edits only.
        let edit = match &payload {
            Payload::ConfigEdit(edit) => Some((edit.state, edit.change.clone(), edit.resolves)),
            _ => None,
        };
        let seq = {
            let mut log = self.log.lock().unwrap();
            let seq = log.append(turn, payload)?;
            self.observe_operations(&log);
            seq
        };
        if let Some((state, change, resolves)) = edit {
            match state {
                // A committed edit takes effect: fold it into the effective policy so a gate reads
                // current policy in O(1) (no log re-scan), and — for session-visible keys — notify.
                EditState::Committed => {
                    {
                        let mut eff = self.effective.lock().unwrap();
                        let mut role = (**eff).clone();
                        crate::policy::apply_config_change(&mut role, &change);
                        *eff = Arc::new(role);
                    }
                    // Resolving a proposal: drop the referenced proposal from the pending queue.
                    // `None` for a direct synchronous/fs-watch commit, which had no queued proposal.
                    if let Some(proposal) = resolves {
                        self.resolve_proposal(proposal);
                    }
                    // Session-visible edits (`scope`, `paths`) surface as an `Ambient` notification
                    // so the session learns its perimeter moved — an optional tail append that never
                    // re-renders the cached preamble (docs/permissions.md). It rides
                    // the same inbox/triage machinery as any notification, and fires for *every*
                    // committing initiator because this is the single appender. `commands`/`tools`
                    // notify nothing — their effect reaches the model only through the classifier.
                    if let Some((body, slot)) = session_visible_notice(&change) {
                        self.notify("config".to_string(), body, Some(slot));
                    }
                }
                // A proposal is inert in the effective config: it joins the pending-proposals queue,
                // addressed by its own `seq`, awaiting an orchestrator's commit/cancel.
                EditState::Proposed => {
                    self.proposals
                        .lock()
                        .unwrap()
                        .push(PendingProposal { seq, change });
                }
                // A cancel drops the referenced proposal from the queue; the effective config
                // never moved.
                EditState::Cancelled => {
                    if let Some(proposal) = resolves {
                        self.resolve_proposal(proposal);
                    }
                }
            }
        }
        self.appended.notify_waiters();
        Ok(seq)
    }

    /// Publish an ephemeral pre-commit output delta to the control stream's `output`
    /// subscribers (docs/control-api.md). Intentionally lossy: a send with no
    /// subscribers — or to a lagging one — is dropped, because the committed entry on
    /// the `log` class is the authoritative copy. Never touches the log.
    pub fn publish_delta(&self, delta: StreamDelta) {
        let _ = self.stream_tx.send(delta);
    }

    /// A fresh subscription to the delta fan-out for one control-stream connection.
    pub(super) fn subscribe_stream(&self) -> broadcast::Receiver<StreamDelta> {
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
        span: crate::consolidate::Span,
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
                        calls.iter().any(|c| c.name == crate::consolidate::NAME)
                    }),
                    _ => false,
                })
            {
                return Ok(false);
            }
            let turn = entries.last().map_or(0, |e| e.turn);
            log.append(
                turn,
                Payload::Summary(crate::log::Summary::consolidation(
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
            .map(|e| e.turn)
            .unwrap_or(0)
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
    pub(crate) fn consented_calls(&self) -> Vec<crate::log::ToolCall> {
        crate::spotter::precedent::consented(self.log.lock().unwrap().entries())
    }
}
