//! Transcript construction: the builder methods a scenario uses to stage log
//! entries, running tools, and approval gates before rendering.

use super::*;

impl Screenplay<'_> {
    /// Commit a [`Payload`] to the transcript as the next sequential log entry,
    /// exactly as the durable `/events` tail would deliver it. `seq` auto-increments
    /// so scenarios read as a script rather than bookkeeping.
    pub(crate) fn commit(&mut self, payload: Payload) -> &mut Self {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.event(TuiEvent::Entry(LogEntry {
            seq,
            turn: 1,
            at: Timestamp(0),
            payload,
        }))
    }

    /// Replay recorded log entries through the production event handler — the
    /// reachability harness (docs/tui.md). Each entry is fed exactly as the durable
    /// `/events` tail delivers it, carrying its own `seq` (unlike [`commit`](Self::commit),
    /// which auto-assigns one). Pair with `persist::load` of a real `.efference/*.jsonl`
    /// and slice `&entries[..=n]` to snapshot a *provably reachable* state at index
    /// `n` — one recording becomes a filmstrip, which is the guarantee a hand-authored
    /// scenario can't give.
    pub(crate) fn replay(&mut self, entries: &[LogEntry]) -> &mut Self {
        for entry in entries {
            self.event(TuiEvent::Entry(entry.clone()));
        }
        self
    }

    /// Run the scenario as a **read-only view** of the recorded session `label`
    /// (docs/tui.md — `efference view`), the way `efference view` launches it: no
    /// composer, no loop verbs, focus resting on the strip. Pair with
    /// [`replay`](Self::replay) to snapshot what opening a real log actually shows.
    pub(crate) fn viewing(&mut self, label: &str) -> &mut Self {
        self.state.set_viewing(label);
        self
    }

    /// A committed user turn (markdown prose).
    pub(crate) fn user(&mut self, text: &str) -> &mut Self {
        self.commit(Payload::User(vec![ContentBlock::Text(text.into())]))
    }

    /// A committed assistant turn: markdown prose plus zero or more tool calls.
    pub(crate) fn assistant(&mut self, text: &str, calls: Vec<ToolCall>) -> &mut Self {
        self.commit(Payload::Assistant {
            thinking: vec![],
            text: vec![ContentBlock::Text(text.into())],
            calls,
        })
    }

    /// A committed assistant turn that did extended thinking first.
    pub(crate) fn assistant_thinking(&mut self, thinking: &str, text: &str) -> &mut Self {
        self.commit(Payload::Assistant {
            thinking: vec![ContentBlock::Thinking {
                text: thinking.into(),
                signature: String::new(),
            }],
            text: vec![ContentBlock::Text(text.into())],
            calls: vec![],
        })
    }

    /// A committed tool exchange in one line: the assistant's `bash`-shaped call
    /// plus its (non-error, raw) result — the shape most transcript scenarios need
    /// a focusable unit to be.
    pub(crate) fn tool_exchange(
        &mut self,
        call_id: &str,
        name: &str,
        command: &str,
        output: &str,
    ) -> &mut Self {
        let call = tool_call(name, call_id, serde_json::json!({ "command": command }));
        self.assistant("", vec![call]);
        self.tool_result(call_id, output, false, WireState::Raw)
    }

    /// Put a tool call **in flight**: commit the assistant turn that makes it, with
    /// no paired result, and report the loop as running. This is exactly the shape
    /// the daemon leaves the log in while a tool executes (`src/agent.rs` appends
    /// the assistant entry before running the call), so it is what
    /// [`in_flight_call`](super::InteractiveState::in_flight_call) reads — the
    /// control strip's foreground-bash-vs-other-tool distinction depends on it.
    pub(crate) fn running_tool(&mut self, call_id: &str, name: &str, input: Value) -> &mut Self {
        self.assistant("", vec![tool_call(name, call_id, input)]);
        self.event(TuiEvent::Running(true))
    }

    /// The common case of [`running_tool`](Self::running_tool): a foreground `bash`
    /// command mid-run — the one state that offers `background` and `kill`.
    pub(crate) fn running_bash(&mut self, call_id: &str, command: &str) -> &mut Self {
        self.running_tool(call_id, crate::bash::NAME, json!({ "command": command }))
    }

    /// A committed spotter verdict for `call_id` (docs/permissions.md) — the `Spot` audit
    /// entry the spotter logs for *every* judged call, cleared ones included, not only the
    /// gated ones a human ever sees a prompt for.
    pub(crate) fn spot(&mut self, call_id: &str, stance: Stance, reason: &str) -> &mut Self {
        self.commit(Payload::Spot(crate::log::Spot {
            call_id: CallId(call_id.into()),
            stance,
            reason: reason.into(),
            origin: crate::log::Origin::Model,
            probed_prefix: None,
        }))
    }

    /// A committed tool result for `call_id`, projected at its `wire` state.
    pub(crate) fn tool_result(
        &mut self,
        call_id: &str,
        content: &str,
        is_error: bool,
        wire: WireState,
    ) -> &mut Self {
        self.commit(Payload::ToolResult {
            call_id: CallId(call_id.into()),
            raw: RawResult {
                content: content.into(),
                is_error,
            },
            wire,
        })
    }

    /// Mark the agent blocked on approval for `call_id` — the pending gate the
    /// daemon's `/state` poll surfaces, driven through the real `Gate` event so the
    /// inline gate renders exactly as in the app (docs/tui.md).
    pub(crate) fn pending_gate(
        &mut self,
        call_id: &str,
        name: &str,
        input: serde_json::Value,
    ) -> &mut Self {
        self.pending_gate_reason(call_id, name, input, None)
    }

    /// A pending gate carrying the spotter's reason — the spotter-routed case, where
    /// the gate renders the reason inline (docs/permissions.md).
    pub(crate) fn pending_gate_reason(
        &mut self,
        call_id: &str,
        name: &str,
        input: serde_json::Value,
        reason: Option<&str>,
    ) -> &mut Self {
        self.event(TuiEvent::Gate(Some(crate::tui::PendingGate {
            call_id: call_id.into(),
            name: name.into(),
            input,
            reason: reason.map(String::from),
        })))
    }

    /// Flip a tool result to its raw output (docs/tui.md) —
    /// the view-state toggle that survives re-projection, here driven directly by
    /// `call_id` rather than through keyboard focus.
    pub(crate) fn show_raw(&mut self, call_id: &str) -> &mut Self {
        self.state.view.toggle_raw(call_id);
        self.state.reproject();
        self
    }
}
