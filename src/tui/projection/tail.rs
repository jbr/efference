//! The live pre-commit tail and the per-result raw-vs-distilled view state
//! (docs/tui.md).

use super::*;

/// The in-flight streaming deltas not yet committed to the log: the assistant's
/// prose so far this turn, and each running tool's accumulated output (keyed by
/// `call_id`, kept in arrival order).
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
    /// What the model is doing *right now*, when that isn't emitting prose
    /// (docs/tui.md). `None` between turns and while prose streams — prose is
    /// its own evidence of progress, so it needs no phase line.
    pub phase: Option<Phase>,
}

/// A generation phase with no prose to show for it. Both of these used to produce **no output at
/// all**: the streaming sink only carried `text_delta`, so a turn spent thinking or emitting a
/// large tool-call input looked identical to a hung harness — and the serve log's last line was
/// often `idle`, which reads as *finished*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// A tool call is being emitted: named at `content_block_start`, then its JSON `input`
    /// accumulating. The input is parsed as a *prefix* — what may be claimed from a partly
    /// arrived document is decided positionally (docs/tui.md).
    ///
    /// The only phase left. Thinking used to be one too, which is what made it
    /// ephemeral: a phase is a single slot, so this variant replaced it.
    CallingTool { name: String, input: String },
}

impl LiveTail {
    /// How many lines the in-flight tool input currently decodes to — test support
    /// for measuring what a growing streaming block costs.
    #[cfg(test)]
    pub(crate) fn phase_input_lines(&self) -> usize {
        match &self.phase {
            Some(Phase::CallingTool { input, .. }) => input.matches("\\n").count(),
            _ => 0,
        }
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
                // The committed entry *is* the record of what the turn produced, so the live
                // phase has nothing left to report.
                self.phase = None;
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
/// One toggle so far. There was a second — expand/collapse — until entries stopped
/// being truncated; the raw fork survives because it is a choice between two real
/// forms of the content, not a choice about how much of one to show.
#[derive(Debug, Default)]
pub(crate) struct ViewState {
    /// `call_id`s the human flipped to **raw** output — the wire distillate is the
    /// default, raw is shown on demand (both are already client-side; this is a
    /// pure render choice, no round-trip).
    raw: HashSet<String>,
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
