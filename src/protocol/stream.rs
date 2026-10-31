//! The control stream's `output` class (docs/control-api.md): the pre-commit fragments a
//! session publishes while a response, a tool, a process, or a side request is still
//! being written. A client renders them live and replaces them with the committed log
//! entry; none is ever logged.

/// A live, pre-commit output fragment streamed to clients as the control stream's
/// `output` class (docs/control-api.md). **Ephemeral and transport-only**:
/// never appended to the log, never projected, and droppable for a slow consumer.
/// The authoritative text always lands as a committed
/// [`LogEntry`](crate::log::LogEntry) on `/events`, so the stream is a live convenience
/// with the durable log as its backstop — which is exactly why it can ride a lossy
/// `broadcast` where `/events` must tail the log.
/// Every model-stream variant carries the content-block `index` it belongs to, and
/// `ToolCallStart` additionally the `tool_use` id: a fragment is partial delivery *of a
/// specific item*, and a client that cannot tell which holds one slot per kind and loses the
/// first tool call's arriving input the moment a second opens. [`ToolOutput`](Self::ToolOutput)
/// and [`ProcessOutput`](Self::ProcessOutput) key on their own ids for the same reason
/// (docs/control-api.md).
#[derive(Clone)]
pub enum StreamDelta {
    /// A fragment of assistant prose as the model streams it from the upstream SSE.
    AssistantText { index: usize, text: String },
    /// A fragment of extended-thinking text as the model streams it. A separate variant from
    /// [`AssistantText`](Self::AssistantText) because it is *not* the assistant's answer — a client
    /// must render it as thinking or not at all, never as prose.
    ThinkingText { index: usize, text: String },
    /// The model began emitting a tool call, named before its arguments stream. Together with
    /// [`ThinkingText`](Self::ThinkingText) these are the output kinds that make a turn visible at all:
    /// without them only prose reaches the stream, so a turn spent thinking or emitting a large
    /// tool input publishes nothing and reads as a hung harness (docs/transport.md).
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    /// A fragment of a tool call's JSON input, verbatim. The bytes are passed through rather than
    /// summarized as a count: they are a partial slice of a JSON document, so a client must not
    /// *parse* them as if complete — but showing what actually arrived claims no structure, and
    /// `{"command":"cargo test` tells an operator far more than a byte count does. (Rendering a
    /// partial document *structurally* is a real option — `jiter`'s partial mode, `actson`'s push
    /// parser — and is filed as its own design question.)
    ToolCallInput { index: usize, fragment: String },
    /// A chunk of a running tool's combined output, tagged by its `tool_use` id.
    ToolOutput { call_id: String, chunk: String },
    /// A chunk of a running background process's output, tagged by the process's
    /// spawn-minted id — rendered `p#<n>` in prose, bare here (docs/process-monitor.md).
    /// Demuxed by id exactly like
    /// [`ToolOutput`](Self::ToolOutput) is by `call_id`. Lossy: a cold-opened client
    /// seeds its tail from the process resource and layers these on top.
    ProcessOutput { id: u64, chunk: String },
    /// A fragment of the summary a compaction is writing (docs/summary.md).
    /// Not the main loop's response: it belongs to no content block of one, and the
    /// committed `Assistant` entry that retires the block variants does not retire it.
    /// The finished text lands as the compaction's `Summary`; these are lossy and never
    /// logged.
    CompactionText { text: String },
    /// A fragment of the compaction's extended thinking, the counterpart of
    /// [`CompactionText`](Self::CompactionText).
    CompactionThinking { text: String },
    /// A fragment of a detached consolidation account (docs/consolidate.md).
    /// Its frozen range and settlement are recorded by consolidation telemetry;
    /// it is not a main-response block and is never logged.
    ConsolidationText { text: String },
    /// Extended thinking from the same detached consolidation request.
    ConsolidationThinking { text: String },
    /// A fragment of the text a triage request is writing (docs/triage.md), whichever
    /// shape runs it. Not the main loop's response, so it carries no block `index` and a
    /// committed `Assistant` entry does not retire it. `call_ids` are the `tool_use` ids
    /// of the results being triaged; empty for the idle triage, which has none. Lossy and
    /// never logged: the verdict lands as the `Triage` entries.
    TriageText { call_ids: Vec<String>, text: String },
    /// A fragment of the triage request's extended thinking, the counterpart of
    /// [`TriageText`](Self::TriageText).
    TriageThinking { call_ids: Vec<String>, text: String },
}
