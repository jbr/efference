//! Transcript construction: the builder methods a scenario uses to stage log
//! entries, running tools, and approval gates before rendering.

use super::*;
use crate::protocol::status::Escalated;

impl Screenplay<'_> {
    /// Commit a [`Payload`] to the transcript as the next sequential log entry,
    /// exactly as the durable `/events` tail would deliver it. `seq` auto-increments
    /// so scenarios read as a script rather than bookkeeping.
    pub(crate) fn commit(&mut self, payload: Payload) -> &mut Self {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.event(TuiEvent::Entry(LogEntry::new(seq, payload).with_turn(1)))
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
    /// (docs/tui.md — `efference dev view`), the way `efference dev view` launches it: no
    /// composer, no loop verbs, focus resting on the strip. Pair with
    /// [`replay`](Self::replay) to snapshot what opening a real log actually shows.
    pub(crate) fn viewing(&mut self, label: &str) -> &mut Self {
        self.state.set_viewing(label);
        self
    }

    /// A committed user turn (markdown prose), carrying no submission id — the shape of a turn
    /// submitted by some other client.
    pub(crate) fn user(&mut self, text: &str) -> &mut Self {
        self.commit(Payload::user(text))
    }

    /// The daemon committing the **oldest pending echo**: a `User` entry carrying that
    /// submission's id, which is what settles the echo (docs/tui.md — the echo is keyed by
    /// `submission_id`, never by text). Panics when nothing is pending, since the scenario then
    /// means something other than what it says.
    pub(crate) fn commit_pending(&mut self) -> &mut Self {
        let pending = self
            .state
            .pending_turns
            .first()
            .cloned()
            .expect("a pending turn to commit");
        let seq = self.next_seq;
        self.next_seq += 1;
        self.event(TuiEvent::Entry(
            LogEntry::new(seq, Payload::user(pending.text))
                .with_turn(1)
                .with_submission_id(Some(pending.submission_id))
                .with_source(pending.source),
        ))
    }

    /// The daemon delivering the **oldest pending echo** as the reply to an `ask`: a
    /// `ToolResult` for `call_id` carrying that submission's id and text — never a `User`
    /// entry (docs/tui.md). `source` is what a non-operator client's reply carries.
    pub(crate) fn commit_reply(&mut self, call_id: &str, source: Option<&str>) -> &mut Self {
        let pending = self
            .state
            .pending_turns
            .first()
            .cloned()
            .expect("a pending turn to deliver");
        let seq = self.next_seq;
        self.next_seq += 1;
        self.event(TuiEvent::Entry(
            LogEntry::new(
                seq,
                Payload::tool_result(call_id, RawResult::text(pending.text), WireState::Raw),
            )
            .with_turn(1)
            .with_submission_id(Some(pending.submission_id))
            .with_source(source.map(str::to_string)),
        ))
    }

    /// A committed assistant turn: markdown prose plus zero or more tool calls.
    pub(crate) fn assistant(&mut self, text: &str, calls: Vec<ToolCall>) -> &mut Self {
        self.commit(Payload::assistant(
            crate::record::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text(text.into())],
                calls,
            ),
        ))
    }

    /// A committed assistant turn that did extended thinking first.
    pub(crate) fn assistant_thinking(&mut self, thinking: &str, text: &str) -> &mut Self {
        self.commit(Payload::assistant(
            crate::record::log::AssistantOutput::from_legacy_messages(
                vec![ContentBlock::Thinking {
                    text: thinking.into(),
                    signature: String::new(),
                }],
                vec![ContentBlock::Text(text.into())],
                vec![],
            ),
        ))
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
        let call = ToolCall::new(name, call_id, serde_json::json!({ "command": command }));
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
        self.assistant("", vec![ToolCall::new(name, call_id, input)]);
        self.phase(LoopPhase::Requesting)
    }

    /// The Responses-dialect twin of [`running_tool`](Self::running_tool): commit an
    /// assistant turn carrying one `function_call` item whose `arguments` is exactly
    /// the given string, unpaired, with the loop reported running. The string is
    /// passed through verbatim so a scenario can stage a malformed one — the
    /// record-only shape the loop leaves when the provider's arguments do not parse.
    pub(crate) fn running_responses_tool(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> &mut Self {
        self.commit(Payload::assistant(
            crate::record::log::AssistantOutput::Responses(vec![
                crate::record::log::ResponsesOutputItem::FunctionCall {
                    id: Some(format!("fc_{call_id}")),
                    call_id: CallId(call_id.into()),
                    name: name.into(),
                    arguments: arguments.into(),
                    status: Some("completed".into()),
                },
            ]),
        ));
        self.phase(LoopPhase::Requesting)
    }

    /// The common case of [`running_tool`](Self::running_tool): a foreground `bash`
    /// command mid-run — the one state that offers `background` and `kill`.
    pub(crate) fn running_bash(&mut self, call_id: &str, command: &str) -> &mut Self {
        self.running_tool(
            call_id,
            crate::record::names::tools::BASH,
            json!({ "command": command }),
        )
    }

    /// Open an in-flight tool call on the live tail, as `tool_call_start` does: the
    /// content block `index` its input fragments will name, the `call_id` its result
    /// will key on, and the tool's name (docs/control-api.md).
    pub(crate) fn calling(&mut self, index: usize, call_id: &str, name: &str) -> &mut Self {
        self.event(TuiEvent::ToolCallStart {
            index,
            call_id: call_id.into(),
            name: name.into(),
        })
    }

    /// A fragment of the call at content block `index`'s input, as
    /// `tool_call_input` delivers it.
    pub(crate) fn call_input(&mut self, index: usize, fragment: &str) -> &mut Self {
        self.event(TuiEvent::ToolCallInput {
            index,
            fragment: fragment.into(),
        })
    }

    /// A fragment of a running tool's output, as `tool_output` delivers it —
    /// demuxed by `call_id`, which is also the triage entry's durable key.
    pub(crate) fn tool_output(&mut self, call_id: &str, chunk: &str) -> &mut Self {
        self.event(TuiEvent::ToolOutput {
            call_id: call_id.into(),
            chunk: chunk.into(),
        })
    }

    /// A committed spotter verdict for `call_id` (docs/permissions.md) — the `Spot` audit
    /// entry the spotter logs for *every* judged call, cleared ones included, not only the
    /// gated ones a human ever sees a prompt for.
    pub(crate) fn spot(&mut self, call_id: &str, stance: Stance, reason: &str) -> &mut Self {
        self.commit(Payload::Spot(crate::record::log::Spot::new(
            CallId(call_id.into()),
            stance,
            reason,
        )))
    }

    /// A committed tool result for `call_id`, projected at its `wire` state.
    pub(crate) fn tool_result(
        &mut self,
        call_id: &str,
        content: &str,
        is_error: bool,
        wire: WireState,
    ) -> &mut Self {
        let raw = RawResult::new(content, is_error);
        self.commit(Payload::tool_result(call_id, raw, wire))
    }

    /// Give the scenario the deterministic halfblocks image capability
    /// (docs/tui.md — snapshots pin halfblocks; the pixel protocols stuff
    /// escape blobs into cell symbols, which no grid should carry).
    pub(crate) fn halfblock_images(&mut self) -> &mut Self {
        self.state
            .images
            .set_picker(Some(ratatui_image::picker::Picker::halfblocks()));
        self
    }

    /// A committed tool result carrying images (docs/tui.md). An image with
    /// empty `data` models the live entry view — a reference whose bytes are
    /// still a fetch away — and one with inline base64 models an offline log,
    /// which decodes at the event fold and renders immediately.
    pub(crate) fn tool_result_images(
        &mut self,
        call_id: &str,
        content: &str,
        images: Vec<crate::record::log::ResultImage>,
    ) -> &mut Self {
        self.commit(Payload::tool_result(
            call_id,
            RawResult::text(content).with_images(images),
            WireState::Raw,
        ))
    }

    /// Mark the agent blocked on approval for `call_id` — the pending gate the
    /// daemon's `gate` topic carries, driven through the real `Gate` event so the
    /// inline gate renders exactly as in the app (docs/tui.md).
    pub(crate) fn pending_gate(&mut self, call_id: &str, name: &str, input: Value) -> &mut Self {
        self.pending_gate_reason(call_id, name, input, None)
    }

    /// A pending gate carrying the spotter's reason — the spotter-routed case, where
    /// the gate renders the reason inline (docs/permissions.md).
    pub(crate) fn pending_gate_reason(
        &mut self,
        call_id: &str,
        name: &str,
        input: Value,
        reason: Option<&str>,
    ) -> &mut Self {
        let mut held = gate(call_id, name, input);
        held.reason = reason.map(String::from);
        self.topic(TopicUpdate::Gate(Some(held)))
    }

    /// A pending gate that reached the operator by **escalation** (docs/permissions.md):
    /// `source` is the escalator as a turn's `source` spells it (`session[<id>]`,
    /// `mcp[<name>]`), or `None` for the operator's own command line, and `note` is the
    /// message it sent with the call. `to` is the operator, the reader of this gate.
    pub(crate) fn pending_gate_escalated(
        &mut self,
        call_id: &str,
        name: &str,
        input: Value,
        reason: Option<&str>,
        source: Option<&str>,
        note: Option<&str>,
    ) -> &mut Self {
        let mut held = gate(call_id, name, input);
        held.reason = reason.map(String::from);
        held.escalations = vec![Escalated {
            source: source.map(String::from),
            to: None,
            note: note.map(String::from),
        }];
        self.topic(TopicUpdate::Gate(Some(held)))
    }

    /// A pending gate whose hold is a bound `agents__decide` (docs/permissions.md):
    /// the gate carries the child's held move, rendered beside the approval so the
    /// human sees the leaf action rather than an id.
    pub(crate) fn pending_gate_bound(
        &mut self,
        call_id: &str,
        leaf_session: &str,
        leaf_name: &str,
        leaf_input: Value,
        leaf_intent: Option<&str>,
    ) -> &mut Self {
        let mut held = gate(
            call_id,
            crate::protocol::agents::DECIDE_WIRE_NAME,
            serde_json::json!({
                "agent": leaf_session,
                "seq": "4",
                "decision": "allow",
            }),
        );
        held.acting_on = Some(BoundAction {
            name: leaf_name.into(),
            input: leaf_input,
            intent: leaf_intent.map(String::from),
            cwd: "/work".into(),
            project: "/work".into(),
            session: leaf_session.into(),
            chain: vec![leaf_session.into()],
        });
        self.topic(TopicUpdate::Gate(Some(held)))
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

/// A gate holding `call_id`'s `name` call, with nothing else said about it: no reason, no
/// bound action, no grant, no escalation. The one place the harness builds a [`Gate`]
/// literal, so a field added to the topic costs a line here.
pub(crate) fn gate(call_id: &str, name: &str, input: Value) -> Gate {
    Gate {
        call_id: call_id.into(),
        name: name.into(),
        input,
        reason: None,
        acting_on: None,
        grant: None,
        escalations: Vec::new(),
    }
}

/// The `ask` topic naming `call_id`, question `n` of `of`, with no turns handed back. The
/// one place the harness builds an [`AskView`] literal.
pub(crate) fn asking(call_id: &str, n: usize, of: usize) -> AskView {
    AskView {
        call_id: call_id.into(),
        question: "Which one?".into(),
        n,
        of,
        recalled: Vec::new(),
    }
}

/// The `cold` topic: the cache believed expired, and the prompt's size when a turn has
/// reported it.
pub(crate) fn cold(context_tokens: Option<u64>) -> Cold {
    Cold {
        since: Timestamp(0),
        context_tokens,
    }
}

/// A background process the panel lists, spawned now.
pub(crate) fn process(id: u64, command: &str) -> ProcessState {
    ProcessState {
        id,
        command: command.into(),
        call_id: None,
        pid: None,
        spawned_at: Timestamp::now(),
        background: true,
        muted: false,
    }
}

impl Screenplay<'_> {
    /// The `processes` topic holding `processes`.
    pub(crate) fn processes(&mut self, processes: Vec<ProcessState>) -> &mut Self {
        self.topic(TopicUpdate::Processes(processes))
    }

    /// A `process_output` delta for process `id`, as the bridge delivers it.
    pub(crate) fn process_output(&mut self, id: u64, chunk: &str) -> &mut Self {
        self.event(TuiEvent::ProcessOutput {
            id,
            chunk: chunk.into(),
        })
    }

    /// Fix the running and idle seconds the panel shows for process `id`. They derive
    /// from the clock, so a scenario that snapshots them states them.
    pub(crate) fn process_times(
        &mut self,
        id: u64,
        running_secs: u64,
        idle_secs: u64,
    ) -> &mut Self {
        let row = self
            .state
            .process_panel
            .iter_mut()
            .find(|row| row.process.id == id)
            .expect("a listed process");
        row.running_secs = running_secs;
        row.idle_secs = idle_secs;
        self
    }

    /// Fix the seconds since the last upstream request the drawer shows.
    pub(crate) fn since_last_request(&mut self, secs: u64) -> &mut Self {
        self.state.since_last_request = Some(secs);
        self
    }
}

/// A standalone session in `~/code/efference`: no title, no roles, no successor.
pub(crate) fn session_identity(id: &str, model: &str) -> SessionIdentity {
    SessionIdentity {
        id: id.into(),
        title: None,
        model: model.into(),
        cwd: "~/code/efference".into(),
        inference: None,
        successor: None,
    }
}
