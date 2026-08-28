//! The live pre-commit tail and the per-result raw-vs-distilled view state
//! (docs/tui.md).

use super::*;

/// The in-flight streaming deltas not yet committed to the log: the assistant's
/// prose so far this turn, each tool call whose input is still arriving, and each
/// running tool's accumulated output. Both collections are keyed by `call_id` and
/// kept in arrival order.
#[derive(Debug, Default)]
pub(crate) struct LiveTail {
    /// Extended thinking streamed so far this turn. **Accumulated transcript
    /// content, not a phase preview**: it stays on screen once the model moves on
    /// to prose or a tool call, and is superseded by the committed thinking block
    /// when the turn lands (docs/tui.md). It was a phase, and a phase is one slot —
    /// so `tool_call_start` replaced it and the thinking vanished the instant the
    /// model started acting, which on a turn that thinks briefly then calls a tool
    /// is too fast to read.
    pub thinking: String,
    pub assistant: String,
    pub(super) tools: Vec<(String, String)>,
    /// The tool calls whose input is still arriving, in the order the model opened
    /// them (docs/tui.md). A **collection**, not a slot: a response may open more
    /// than one call, and every model-stream delta names the content block it
    /// belongs to (docs/control-api.md), so each call's fragments can be bound to
    /// it. Held one call at a time, the second `tool_call_start` overwrote the
    /// first and the arguments the operator was watching arrive disappeared.
    /// Empty between turns and while prose streams — prose is its own evidence of
    /// progress, so it needs no calling line.
    pub(super) calls: Vec<LiveCall>,
}

/// One tool call being emitted: named at `content_block_start`, then its JSON
/// `input` accumulating fragment by fragment. Without it a turn spent emitting a
/// large tool-call input produced no output at all and looked identical to a hung
/// harness.
///
/// The input is parsed as a *prefix* — what may be claimed from a partly arrived
/// document is decided positionally (docs/tui.md, `projection/partial.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveCall {
    /// The content-block `index` this call's `tool_call_input` fragments name. The
    /// input deltas carry only the index (docs/control-api.md), so this is how a
    /// fragment finds the call it extends.
    pub index: usize,
    /// The `tool_use` id the call's result will key on, carried on
    /// `tool_call_start`. It is what lets a *live* block bind to its call before
    /// commit — a block with no `call_id` has nothing for per-entry view state
    /// (docs/tui.md) to be recorded against, so any toggle on it was dropped by the
    /// next re-projection.
    pub call_id: String,
    pub name: String,
    pub input: String,
}

impl LiveTail {
    /// How many lines the in-flight tool inputs currently decode to — test support
    /// for measuring what a growing streaming block costs.
    #[cfg(test)]
    pub(crate) fn call_input_lines(&self) -> usize {
        self.calls
            .iter()
            .map(|call| call.input.matches("\\n").count())
            .sum()
    }

    /// Open a tool call, or restart one that arrived twice under the same id. The
    /// `call_id` is the identity; `index` is the address its input fragments use.
    pub(crate) fn start_call(&mut self, index: usize, call_id: &str, name: &str) {
        let call = LiveCall {
            index,
            call_id: call_id.to_string(),
            name: name.to_string(),
            input: String::new(),
        };
        match self.calls.iter_mut().find(|c| c.call_id == call_id) {
            Some(existing) => *existing = call,
            None => self.calls.push(call),
        }
    }

    /// Append a fragment to the call at content-block `index`. A fragment for a
    /// block no `tool_call_start` opened is dropped: there is no call to attribute
    /// it to, and guessing the most recent one is what the single slot did.
    pub(crate) fn push_call_input(&mut self, index: usize, fragment: &str) -> bool {
        match self.calls.iter_mut().find(|c| c.index == index) {
            Some(call) => {
                call.input.push_str(fragment);
                true
            }
            None => false,
        }
    }

    /// The calls whose input is still arriving, in the order they opened.
    pub(crate) fn calls(&self) -> &[LiveCall] {
        &self.calls
    }

    /// Drop every in-flight call — what arriving prose does, prose being its own
    /// evidence of progress.
    pub(crate) fn clear_calls(&mut self) {
        self.calls.clear();
    }

    /// Append a fragment of a running tool's output.
    pub(crate) fn push_tool_output(&mut self, call_id: &str, chunk: &str) {
        if let Some((_, buf)) = self.tools.iter_mut().find(|(id, _)| id == call_id) {
            buf.push_str(chunk);
        } else {
            self.tools.push((call_id.to_string(), chunk.to_string()));
        }
    }

    /// A committed entry supersedes the live fragments it was streaming: clear the
    /// assistant buffer when an assistant turn lands, and drop a tool's buffer when
    /// its result lands. (Mirrors `main.rs`'s `streamed_*` dedup flags, now
    /// structural.)
    pub(crate) fn commit(&mut self, entry: &LogEntry) {
        match &entry.payload {
            Payload::Assistant { .. } => {
                self.assistant.clear();
                // The committed entry carries the turn's thinking blocks and renders
                // them the same way, so the live copy has been superseded rather
                // than discarded.
                self.thinking.clear();
                // The committed entry *is* the record of what the turn produced, so the
                // in-flight calls have nothing left to report.
                self.calls.clear();
            }
            Payload::ToolResult { call_id, .. } => {
                self.tools.retain(|(id, _)| id != &call_id.0);
            }
            _ => {}
        }
    }
}

/// Per-result view-state the human has toggled, carried in `tui::state` so it
/// survives re-projection (docs/tui.md). Any entry can be re-projected at any time,
/// so this can't live in the rebuilt [`Document`] — it lives here, keyed by the
/// result's `call_id`, and the projection applies it when building each result.
///
/// Two toggles, one per fork that really exists. The raw fork is a choice between
/// two real forms of a committed result's content; the peek fork is a choice about
/// how much of a *running* tool's unbounded output to show, which is the one place
/// the transcript does not render whole (docs/tui.md).
///
/// Both are keyed by `call_id` for the same reason: it is the only identity that
/// survives a rebuild. A live block that carried no `call_id` had nowhere to record
/// a toggle, so the next streamed fragment re-projected it back to its default.
#[derive(Debug, Default)]
pub(crate) struct ViewState {
    /// `call_id`s the human flipped to **raw** output — the wire distillate is the
    /// default, raw is shown on demand (both are already client-side; this is a
    /// pure render choice, no round-trip).
    raw: HashSet<String>,
    /// `call_id`s whose *running* output is shown in full rather than as the
    /// trailing-lines peek. The call is still in flight, so the entry it applies to
    /// is rebuilt on every output fragment — which is exactly why the flag lives
    /// here and not in the document.
    expanded: HashSet<String>,
    /// `call_id`s toggled since the last projection read this. The incremental
    /// projection (`tui/projection/incremental.rs`) rebuilds only the entries that
    /// own them, so a toggle has to announce itself rather than be diffed for.
    dirty: HashSet<String>,
}

impl ViewState {
    pub(in crate::tui) fn is_raw(&self, call_id: &str) -> bool {
        self.raw.contains(call_id)
    }

    /// Flip raw-vs-wire for a `call_id`, returning whether it now shows raw.
    pub(crate) fn toggle_raw(&mut self, call_id: &str) -> bool {
        self.dirty.insert(call_id.to_string());
        toggle(&mut self.raw, call_id)
    }

    /// Whether an entry's own toggle is currently on — the flag its block-menu
    /// label names ([`menu_items`](crate::tui::block_menu::menu_items)). One place,
    /// so the renderer and the key/mouse dispatch cannot disagree about which fork
    /// an entry kind reads.
    pub(in crate::tui) fn is_flipped(&self, kind: EntryKind, key: Option<&str>) -> bool {
        match (kind, key) {
            (EntryKind::ToolResult, Some(key)) => self.is_raw(key),
            (EntryKind::RunningTool, Some(key)) => self.is_expanded(key),
            _ => false,
        }
    }

    /// Whether a running tool's output is shown in full rather than as a peek.
    pub(in crate::tui) fn is_expanded(&self, call_id: &str) -> bool {
        self.expanded.contains(call_id)
    }

    /// Flip peek-vs-full for a running call's output, returning whether it now
    /// shows the whole output. No `dirty` entry: the block this applies to is in
    /// the live tail, which is rebuilt on every projection anyway — `dirty` names
    /// *committed* entries the incremental projection would otherwise reuse.
    pub(crate) fn toggle_expanded(&mut self, call_id: &str) -> bool {
        toggle(&mut self.expanded, call_id)
    }

    /// Drop a landed call's live-only view state. The peek fork applies to output
    /// still arriving; once the result commits, the entry that replaces it renders
    /// whole and the flag would otherwise sit in the set for the session's life.
    pub(crate) fn forget_live(&mut self, call_id: &str) {
        self.expanded.remove(call_id);
    }

    /// Take the `call_id`s toggled since the last call — the entries the next
    /// projection has to rebuild.
    pub(crate) fn take_dirty(&mut self) -> HashSet<String> {
        std::mem::take(&mut self.dirty)
    }
}

/// Flip a `call_id`'s membership in a set, returning whether it is now present.
pub(in crate::tui) fn toggle(set: &mut HashSet<String>, call_id: &str) -> bool {
    if set.remove(call_id) {
        false
    } else {
        set.insert(call_id.to_string());
        true
    }
}
