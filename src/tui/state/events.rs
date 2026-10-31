//! Inbound `TuiEvent` handling and transcript reprojection (docs/tui.md).

use super::*;

impl<'a> InteractiveState<'a> {
    /// Apply one inbound event: fold it into the log or live tail and flag what the
    /// UI loop must do next. Returns true if the UI should exit.
    ///
    /// This never re-projects or paints itself — it only updates state and sets
    /// [`reproject_pending`](Self::reproject_pending) / [`dirty`](Self::dirty). The
    /// loop drains a whole arriving burst through here, then re-projects and paints
    /// at most once per redraw tick. That deferral is what keeps a reattach replay
    /// (or a fast token stream) from doing O(N²) projection work.
    pub(in crate::tui) fn handle_event(&mut self, event: TuiEvent) -> bool {
        match event {
            TuiEvent::Entry(entry) => self.commit_entry(entry),
            TuiEvent::AssistantDelta(text) => {
                self.live.assistant.push_str(&text);
                // Prose arriving *is* the progress signal, so it supersedes the
                // calling lines. (Prose leads a response's tool calls on the wire,
                // so this clears nothing a turn is still emitting; a response that
                // genuinely interleaves the two is the open ordering question in
                // docs/tui.md, not this line.)
                self.live.clear_calls();
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ThinkingDelta(text) => {
                // Accumulated, not a single-slot preview: it stays in the transcript when the model
                // moves on to acting (docs/tui.md).
                self.live.thinking.push_str(&text);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => {
                // Joins the calls already in flight rather than replacing them: a
                // response may open more than one, and the first call's arriving
                // arguments are what a single slot dropped (docs/tui.md).
                self.live.start_call(index, &call_id, &name);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolCallInput { index, fragment } => {
                if self.live.push_call_input(index, &fragment) {
                    self.mark_reproject();
                }
            }
            TuiEvent::ToolOutput { call_id, chunk } => {
                self.live.push_tool_output(&call_id, &chunk);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::Topic(update) => return self.apply_topic(update),
            TuiEvent::ProcessOutput { id, chunk } => self.push_process_output(id, &chunk),
            TuiEvent::Image { seq, idx, image } => {
                // The reserved footprint was sized from the entry view's metadata,
                // so landing the pixels repaints without a reproject (docs/tui.md).
                self.images.insert((seq, idx), image);
                self.dirty = true;
            }
            TuiEvent::ArtifactUrl { coordinate, url } => {
                if self.transcript.set_artifact_url(&self.log, coordinate, url) {
                    self.mark_reproject();
                }
            }
            TuiEvent::Notice(note) => {
                // Status-line chrome from a dispatched call that answered after the
                // keypress (docs/tui.md) — repaint, no reproject.
                self.ui.debug_message = note.into();
                self.dirty = true;
            }
            TuiEvent::Recalled(withdrawn) => self.apply_recall(&withdrawn),
            TuiEvent::NotDelivered { turn, answered } => self.apply_not_delivered(turn, answered),
            TuiEvent::Uploaded { key, result } => self.upload_landed(key, result),
            TuiEvent::ClipboardImage(found) => {
                self.clipboard_image = found;
                self.dirty = true;
            }
            TuiEvent::Thumbnail { key, image } => self.stage_thumbnail(key, image),
            TuiEvent::Sessions(listing) => {
                self.apply_sessions(listing);
                self.dirty = true;
            }
            #[cfg(feature = "dev")]
            TuiEvent::SessionFormPrototype => self.open_session_form_prototype(),
            TuiEvent::NewSession(context) => {
                self.open_new_session_form(context);
                self.dirty = true;
            }
            TuiEvent::Preview(resolved) => {
                self.apply_preview(resolved);
                self.dirty = true;
            }
            TuiEvent::Catalog { connection, result } => {
                self.apply_catalog(&connection, result);
                self.dirty = true;
            }
            TuiEvent::Created(created) => return self.apply_created(created),
            TuiEvent::Archived(rows) => {
                self.apply_archived(rows);
                self.dirty = true;
            }
            TuiEvent::Resumed(resumed) => {
                self.dirty = true;
                return self.apply_resumed(resumed);
            }
            // A failed start ends its stream after the frame that says why; the view
            // stays, with the reason and the retry (`starting.rs`).
            TuiEvent::Disconnected => {
                self.dirty = true;
                if self.start_failed() {
                    return false;
                }
                self.switch_to.get_or_insert(Exit::Closed);
                return true;
            }
        }
        false
    }

    /// Fold a committed entry into the log.
    fn commit_entry(&mut self, entry: LogEntry) {
        // A committed entry supersedes whatever was streamed live for it.
        self.live.commit(&entry);
        // The peek-vs-full fork belongs to output still arriving; the
        // committed result renders whole, so the flag has nothing left to
        // apply to (docs/tui.md).
        if let Payload::ToolResult { call_id, .. } = &entry.payload {
            self.view.forget_live(&call_id.0);
        }
        self.resolve_result_images(&entry);
        // A committed user turn resolves its optimistic pending echo
        // (docs/tui.md): the entry *is* on the wire now (`project(log)` is exactly what
        // the request sends), so drop the echo carrying that `submission_id`. Keyed by
        // id rather than by text — recall hands a queued turn back to be edited and
        // re-sent, and a content match would then settle the wrong echo.
        // A reply to an `ask` settles it the same way: the daemon puts it in the
        // log as the call's result, carrying the submission's id, never as a
        // `User` entry (docs/tui.md).
        if matches!(entry.payload, Payload::User(_) | Payload::ToolResult { .. })
            && let Some(id) = entry.submission_id.as_deref()
        {
            self.pending_turns.retain(|p| p.submission_id != id);
        }
        // Entries can arrive out of order across the durable/replay boundary;
        // keep the log sorted by seq so the projection is stable.
        match self.log.binary_search_by_key(&entry.seq, |e| e.seq) {
            Ok(idx) => self.log[idx] = entry,
            Err(idx) => self.log.insert(idx, entry),
        }
        self.loading.pending_request = false;
        self.mark_reproject();
    }

    /// Open the new-session form over an empty context.
    #[cfg(feature = "dev")]
    fn open_session_form_prototype(&mut self) {
        // The offline gallery has no host to read a context from, so the
        // form opens against an empty one — the same state the inline
        // `efference dev session-form` container draws (docs/tui.md).
        self.ui_mode = UiMode::Sessions(crate::tui::sessions::SessionMenu {
            form: Some(Box::new(crate::tui::session_form::Form::new(
                crate::tui::session_form::NewSessionContext::default(),
            ))),
            ..Default::default()
        });
        self.dirty = true;
    }

    /// Append process output to the panel's row for `id`.
    fn push_process_output(&mut self, id: u64, chunk: &str) {
        // The panel's tail is built *here*, not received: the `processes` topic
        // carries structure only, so these deltas are the sole source of tail
        // content (docs/control-api.md). An unknown id (a process the panel
        // has not seen yet) is ignored — it appears with the next state change.
        if let Some(row) = self.process_panel.iter_mut().find(|p| p.process.id == id) {
            row.push_output(chunk);
            self.dirty = true;
        }
    }

    /// Show a staged attachment's decoded thumbnail.
    fn stage_thumbnail(&mut self, key: u64, image: ::image::DynamicImage) {
        // A chip removed before its thumbnail decoded has nowhere to show it.
        if self.staged.items().iter().any(|s| s.key == key) {
            self.staged_images.insert((key, 0), Ok(image));
            self.dirty = true;
        }
    }

    /// Start resolving a committed entry's result images (docs/tui.md). An image
    /// that rode the entry itself (an offline log via `persist::load`) decodes in
    /// place; a data-omitted reference (every live entry view,
    /// docs/control-api.md) is requested from the tokio fetcher, which answers
    /// with a [`TuiEvent::Image`]. Slot presence gates both, so a re-delivered
    /// entry (a reattach replay) resolves nothing twice.
    fn resolve_result_images(&mut self, entry: &LogEntry) {
        let Payload::ToolResult { raw, .. } = &entry.payload else {
            return;
        };
        for (idx, image) in raw.images.iter().enumerate() {
            let key = (entry.seq, idx);
            if !self.images.needs_fetch(key) {
                continue;
            }
            if image.data.is_empty() {
                self.images.note_fetching(key);
                let _ = self.images_tx.send(key);
            } else {
                self.images.insert_inline(key, &image.data);
            }
        }
    }

    /// Flag that the transcript must be re-projected before the next paint.
    pub(super) fn mark_reproject(&mut self) {
        self.reproject_pending = true;
        self.dirty = true;
    }

    /// Re-project the transcript from the log + live tail if a change is pending,
    /// then clear the flag. Called once before each paint, so a drained burst of
    /// events collapses to a single projection. The cached layout is invalidated;
    /// the next frame recomputes `document_height` and the UI loop re-pins the
    /// bottom if [`stuck_to_bottom`](ViewportState::stuck_to_bottom).
    pub(in crate::tui) fn flush_reproject(&mut self) {
        if self.reproject_pending {
            self.reproject();
            self.reproject_pending = false;
        }
    }

    /// Bring the transcript document up to date with the log + live tail.
    /// Prefer [`flush_reproject`](Self::flush_reproject) on the hot path; this is
    /// for callers that must reproject immediately (e.g. tests, an expand action).
    ///
    /// The update is incremental (docs/tui.md): committed entries keep their nodes —
    /// and, through them, their cached layout — unless a call they own changed, so a
    /// streamed token costs the tail rather than the whole log.
    pub(in crate::tui) fn reproject(&mut self) {
        // Timed only when `--frame-cost` asked for it (`frame_cost.rs`): this runs
        // before the paint rather than inside it, so without attributing it here a
        // frame's reported cost would omit rebuilding the nodes it then lays out.
        let started = self.frame_cost.is_some().then(Instant::now);
        let held = self.topics.gate.as_ref();
        let gate = projection::Gate {
            call: held.map(|g| g.call_id.as_str()),
            ask: self.topics.ask.as_ref().map(|a| a.call_id.as_str()),
            reason: held.and_then(|g| g.reason.as_deref()),
            acting_on: held.and_then(|g| g.acting_on.as_ref()),
        };
        self.transcript.update(
            &mut self.document,
            &self.log,
            &self.live,
            &mut self.view,
            gate,
            &self.pending_turns,
        );
        self.viewport.cached_layout = None;
        if let (Some(started), Some(cost)) = (started, self.frame_cost.as_mut()) {
            cost.note_projection(started.elapsed().as_micros().min(u32::MAX as u128) as u32);
        }
    }
}
