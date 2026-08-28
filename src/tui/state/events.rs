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
            TuiEvent::Entry(entry) => {
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
                if matches!(entry.payload, Payload::User(_))
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
                // Accumulated, not a phase: it stays in the transcript when the model
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
            TuiEvent::Gate(gate) => {
                // The gate renders inline in the transcript (the tail call shows
                // `⏸ awaiting approval`, docs/tui.md), so a gate change must
                // reproject — unlike the other ephemeral chrome (processes/usage). The
                // status bar still mirrors the affordance for the scrolled-away case.
                self.ui.debug_message = match &gate {
                    Some(g) => {
                        format!("approval needed: {} — ⏎ approve · type to redirect", g.name).into()
                    }
                    // Gate resolved — drop the stale "approval needed" hint.
                    None => IDLE_STATUS.into(),
                };
                // A stager edits an argument *for* the pending gate; if the gate
                // resolves out from under it (another client, a daemon-side abort),
                // committing would fall through to a normal turn submit — cancel
                // instead and restore the parked draft.
                if gate.is_none() && self.staging.is_some() {
                    self.cancel_staging();
                }
                self.pending_gate = gate;
                self.mark_reproject();
            }
            TuiEvent::Processes(mut processes) => {
                // Ephemeral chrome, not transcript content — repaint, no reproject
                // (the running tail is never in the log, docs/supervisor.md).
                //
                // Carry each process's accumulated tail across the replacement. The
                // `processes` topic is structure only — it cannot carry the tail without
                // re-emitting per output chunk (docs/control-api.md) — so this client is
                // the only holder of it, and a wholesale assignment would wipe what the
                // `process_output` deltas built. Matched by id; a process that has since
                // exited simply drops out with its tail.
                for proc in &mut processes {
                    if let Some(prev) = self.processes.iter().find(|p| p.id == proc.id) {
                        proc.tail.clone_from(&prev.tail);
                        proc.last_chunk_at = prev.last_chunk_at;
                        proc.refresh_elapsed();
                    }
                }
                self.processes = processes;
                self.dirty = true;
            }
            TuiEvent::ProcessOutput { id, chunk } => {
                // The panel's tail is built *here*, not received: the `processes` topic
                // carries structure only, so these deltas are the sole source of tail
                // content (docs/control-api.md). An unknown id (a process the panel
                // has not seen yet) is ignored — it appears with the next state change.
                // Bound the accumulation so a chatty process can't grow it without
                // limit; only the last line is shown anyway.
                if let Some(p) = self.processes.iter_mut().find(|p| p.id == id) {
                    p.last_chunk_at = Some(crate::log::Timestamp::now().0);
                    p.refresh_elapsed();
                    p.tail.push_str(&chunk);
                    if p.tail.len() > BG_TAIL_CAP {
                        let from = p.tail.len() - BG_TAIL_CAP;
                        // Trim on a char boundary so the panel never slices a codepoint.
                        let from = (from..=p.tail.len())
                            .find(|&i| p.tail.is_char_boundary(i))
                            .unwrap_or(p.tail.len());
                        p.tail.drain(..from);
                    }
                    self.dirty = true;
                }
            }
            TuiEvent::Usage(usage) => {
                // Ephemeral telemetry, not transcript content — update and redraw, no
                // reproject. Usage stats are rendered in the status bar.
                self.usage = Some(usage);
                return false;
            }
            TuiEvent::UpstreamFailure(failure) => {
                // Ephemeral transport diagnostic, not transcript content — update and
                // repaint the status bar without reprojection.
                self.upstream_failure = failure;
                self.dirty = true;
            }
            TuiEvent::UpstreamRetry(retry) => {
                // Same class as the failure diagnostic: status-bar chrome, no
                // reprojection. `None` is the all-clear and removes the indicator.
                self.upstream_retry = retry;
                self.dirty = true;
            }

            TuiEvent::Stall(stall) => {
                // Status-bar chrome like the two transport diagnostics above;
                // `None` means a trajectory is running again.
                self.stall = stall;
                self.dirty = true;
            }

            TuiEvent::SessionIdentity(identity) => {
                // Sidebar chrome (the SESSION section), not transcript content —
                // update and repaint, no reproject.
                self.session_identity = identity;
                self.dirty = true;
            }

            TuiEvent::Policy(policy) => {
                // Ephemeral chrome (the sidebar POLICY section), not transcript content —
                // update and repaint, no reproject.
                self.policy = Some(policy);
                self.dirty = true;
            }
            TuiEvent::Pace(pace) => {
                // Operator-view chrome (the status-bar pace indicator), not transcript content —
                // update and repaint, no reproject (docs/tui.md). Reconciles
                // the optimistic update a pace key made with the daemon's authoritative pace.
                self.pace = Some(pace);
                self.dirty = true;
            }
            TuiEvent::Running(running) => {
                // Not rendered; it's the "is the loop working" signal graduated Esc reads
                // (docs/tui.md). No repaint needed — nothing on screen depends on it directly.
                self.loop_running = running;
                return false;
            }
            TuiEvent::Image { seq, idx, image } => {
                // The reserved footprint was sized from the entry view's metadata,
                // so landing the pixels repaints without a reproject (docs/tui.md).
                self.images.insert((seq, idx), image);
                self.dirty = true;
            }
            TuiEvent::Notice(note) => {
                // Status-line chrome from a dispatched call that answered after the
                // keypress (docs/tui.md) — repaint, no reproject.
                self.ui.debug_message = note.into();
                self.dirty = true;
            }
            TuiEvent::Recalled(withdrawn) => self.apply_recall(withdrawn),
            TuiEvent::Disconnected => return true,
        }
        false
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

    /// Recompute every clock-derived display value from the absolute stamps it was
    /// derived from: each process row's running/idle seconds and the status bar's
    /// seconds-since-last-request. Returns whether anything moved.
    ///
    /// Called once a second by the UI loop's clock tick. The wire stopped carrying
    /// ticking values so the level-triggered `state` topics could stay quiet
    /// (docs/control-api.md); the elapsed *display* still has to advance, and this is
    /// where it does — keeping rendering itself a pure function of state, which the
    /// snapshot harness depends on.
    pub(in crate::tui) fn refresh_clocks(&mut self) -> bool {
        let mut changed = false;
        for p in &mut self.processes {
            let before = (p.running_secs, p.idle_secs);
            p.refresh_elapsed();
            changed |= (p.running_secs, p.idle_secs) != before;
        }
        if let Some(usage) = &mut self.usage {
            let secs = usage.last_request_at.map(crate::tui::secs_since);
            if usage.seconds_since_last_request != secs {
                usage.seconds_since_last_request = secs;
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
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
        let started = self.frame_cost.is_some().then(std::time::Instant::now);
        let gate = projection::Gate {
            call: self.pending_gate.as_ref().map(|g| g.call_id.as_str()),
            reason: self.pending_gate.as_ref().and_then(|g| g.reason.as_deref()),
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
