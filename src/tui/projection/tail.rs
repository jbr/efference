//! The live pre-commit tail and the per-view expand/collapse state (docs/tui.md).

use super::*;

/// The in-flight streaming deltas not yet committed to the log: the assistant's
/// prose so far this turn, and each running tool's accumulated output (keyed by
/// `call_id`, kept in arrival order).
#[derive(Debug, Default)]
pub(crate) struct LiveTail {
    pub assistant: String,
    pub(super) tools: Vec<(String, String)>,
    /// What the model is doing *right now*, when that isn't emitting prose
    /// (backlog/dogfooding-friction.md). `None` between turns and while prose streams — prose is
    /// its own evidence of progress, so it needs no phase line.
    pub phase: Option<Phase>,
}

/// A generation phase with no prose to show for it. Both of these used to produce **no output at
/// all**: the streaming sink only carried `text_delta`, so a turn spent thinking or emitting a
/// large tool-call input looked identical to a hung harness — and the serve log's last line was
/// often `idle`, which reads as *finished*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Extended thinking, with the text so far. Shown as a live preview, not accumulated
    /// transcript content: it is cleared when the turn commits (rendering committed thinking in
    /// the transcript is a separate unit — docs/transport.md).
    Thinking { text: String },
    /// A tool call is being emitted: named at `content_block_start`, then its JSON `input`
    /// accumulating. The text is shown verbatim and never parsed — it is a partial slice of a JSON
    /// document until the block closes. Showing it claims no structure, and `{"command":"cargo
    /// test` says far more than a byte count; parsing a partial document *structurally* is its own
    /// design question (backlog/tui-streaming-lifecycle.md).
    CallingTool { name: String, input: String },
}

impl LiveTail {
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

/// Per-exchange view-state the human has toggled, carried in `tui::state` so it
/// survives re-projection (docs/tui.md). The transcript rebuilds on every
/// streamed token, so a block's collapse/raw state can't live in the rebuilt
/// [`Document`] — it lives here, keyed by the result's `call_id`, and `project`
/// applies it when building each exchange.
#[derive(Debug, Default)]
pub(crate) struct ViewState {
    /// `call_id`s whose exchange the human expanded (`▼` Full instead of `▶` Brief).
    expanded: HashSet<String>,
    /// `call_id`s the human flipped to **raw** output — the wire distillate is the
    /// default, raw is shown on demand (both are already client-side; this is a
    /// pure render choice, no round-trip).
    raw: HashSet<String>,
}

impl ViewState {
    pub(in crate::tui) fn is_expanded(&self, call_id: &str) -> bool {
        self.expanded.contains(call_id)
    }

    pub(in crate::tui) fn is_raw(&self, call_id: &str) -> bool {
        self.raw.contains(call_id)
    }

    /// Flip the expand state for a `call_id`, returning the new state. The
    /// [`Document`]'s level is recomputed from this on the next projection.
    pub(crate) fn toggle_expanded(&mut self, call_id: &str) -> bool {
        toggle(&mut self.expanded, call_id)
    }

    /// Flip raw-vs-wire for a `call_id`, returning whether it now shows raw.
    pub(crate) fn toggle_raw(&mut self, call_id: &str) -> bool {
        toggle(&mut self.raw, call_id)
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
