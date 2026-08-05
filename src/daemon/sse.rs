//! Server-Sent Events plumbing for `/stream` (docs/control-channel.md).
//!
//! `/events` is served by `stream.rs` instead: its frames span three durability classes
//! and must be ordered against each other, which a log-only tail cannot express.

use super::*;

/// One ephemeral [`StreamDelta`] as an SSE event. **No `id`** — deltas are
/// pre-commit and carry no `seq`; that absence is the whole reason they ride a
/// side channel rather than `/events`, whose `id == seq` is a durable resume
/// cursor. The body is always JSON so a fragment containing newlines can't perturb
/// SSE's line-oriented `data:` framing.
pub(crate) struct DeltaEvent {
    /// The delta kind → the SSE `event:` field, so a client switches without
    /// parsing the body first (`assistant_delta` / `tool_output` / `bg_output`).
    kind: &'static str,
    data: String,
}

impl Eventable for DeltaEvent {
    fn data(&self) -> &str {
        &self.data
    }
    fn event_type(&self) -> Option<&str> {
        Some(self.kind)
    }
    fn id(&self) -> Option<&str> {
        None
    }
}

pub(crate) fn delta_event(delta: &StreamDelta) -> DeltaEvent {
    match delta {
        StreamDelta::AssistantText(text) => DeltaEvent {
            kind: "assistant_delta",
            data: serde_json::json!({ "text": text }).to_string(),
        },
        StreamDelta::ThinkingText(text) => DeltaEvent {
            kind: "thinking_delta",
            data: serde_json::json!({ "text": text }).to_string(),
        },
        StreamDelta::ToolCallStart { name } => DeltaEvent {
            kind: "tool_call_start",
            data: serde_json::json!({ "name": name }).to_string(),
        },
        StreamDelta::ToolCallInput { fragment } => DeltaEvent {
            kind: "tool_call_input",
            data: serde_json::json!({ "fragment": fragment }).to_string(),
        },
        StreamDelta::ToolOutput { call_id, chunk } => DeltaEvent {
            kind: "tool_output",
            data: serde_json::json!({ "call_id": call_id, "chunk": chunk }).to_string(),
        },
        StreamDelta::BgOutput { handle, chunk } => DeltaEvent {
            kind: "bg_output",
            data: serde_json::json!({ "handle": handle, "chunk": chunk }).to_string(),
        },
    }
}

/// A per-connection stream of ephemeral output deltas (docs/README.md). Subscribes to
/// the broadcast and forwards each delta as an SSE event. A consumer that lags past
/// the ring drops the oldest deltas (`Lagged`) and continues — acceptable precisely
/// because the durable log (`/events`) is the authoritative backstop, so the stream
/// trades completeness for never stalling the producing loop.
pub(crate) fn stream_deltas(
    shared: Arc<Shared>,
) -> Pin<Box<dyn Stream<Item = DeltaEvent> + Send + Sync + 'static>> {
    let rx = shared.subscribe_stream();
    Box::pin(futures_lite::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(delta) => return Some((delta_event(&delta), rx)),
                // Fell behind the ring: skip the gap and keep tailing live.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                // All senders dropped (daemon tearing down): end the stream.
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }))
}
