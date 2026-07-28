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
                // A committed user turn resolves its optimistic pending echo
                // (docs/tui.md): the entry *is* on the wire now
                // (`project(log)` is exactly what the request sends), so drop the matching
                // queued echo. First-match on the ordered queue keeps ordering correct even
                // when the same message was sent twice.
                if let Payload::User(blocks) = &entry.payload {
                    let text = projection::join_text(blocks);
                    if let Some(pos) = self.pending_turns.iter().position(|p| *p == text) {
                        self.pending_turns.remove(pos);
                    }
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
                // Prose arriving *is* the progress signal, so it supersedes any phase line.
                self.live.phase = None;
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ThinkingDelta(text) => {
                match &mut self.live.phase {
                    Some(Phase::Thinking { text: buf }) => buf.push_str(&text),
                    _ => self.live.phase = Some(Phase::Thinking { text }),
                }
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolCallStart { name } => {
                // Replaces a thinking phase: the model has moved on to acting.
                self.live.phase = Some(Phase::CallingTool {
                    name,
                    input: String::new(),
                });
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::ToolCallInput { fragment } => {
                if let Some(Phase::CallingTool { input, .. }) = &mut self.live.phase {
                    input.push_str(&fragment);
                    self.mark_reproject();
                }
            }
            TuiEvent::ToolOutput { call_id, chunk } => {
                self.live.push_tool_output(&call_id, &chunk);
                self.loading.pending_request = true;
                self.mark_reproject();
            }
            TuiEvent::Gate(gate) => {
                // The gate renders inline in the transcript (the tail exchange shows
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
            TuiEvent::Processes(processes) => {
                // Ephemeral chrome, not transcript content — repaint, no reproject
                // (the running tail is never in the log, docs/supervisor.md).
                self.processes = processes;
                self.dirty = true;
            }
            TuiEvent::BgOutput { handle, chunk } => {
                // Smooth the panel's tail between `/state` polls (TUI-bg-2). Append to
                // the matching row; the next poll reconciles to the authoritative
                // snapshot. An unknown handle (process spawned since the last poll) is
                // ignored — it appears within one poll interval. Bound the appended
                // tail so a chatty process can't grow it without limit before the next
                // poll trims it; only the last line is shown anyway.
                if let Some(p) = self.processes.iter_mut().find(|p| p.handle == handle) {
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
            TuiEvent::Disconnected => return true,
        }
        false
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
    }
}
