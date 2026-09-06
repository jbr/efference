//! `project(log) -> Vec<ApiMessage>` — the deterministic view sent to the API
//! (docs/README.md, docs/log-and-projection.md).
//!
//! This must be **byte-stable**: identical input serializes to identical bytes
//! every time, or prompt caching breaks silently. The two hazards
//! (docs/log-and-projection.md) are non-deterministic map ordering — avoided by
//! using [`serde_json::Value`], which serializes object keys in sorted order —
//! and any change to field order or naming on the wire, which the
//! expected-bytes test in `test.rs` pins.

use serde::Serialize;
use serde_json::Value;

use std::collections::HashSet;

use crate::log::{Keep, LogEntry, Mark, MarkKind, MessagesOutputBlock, Payload, Seq};

/// A boundary mark and the summary it contributes, extracted in the pre-pass.
struct Boundary {
    /// The `upto` that the mark named — the last seq of the elided old-task span.
    /// The summary is injected just before the first non-elided entry with
    /// `seq > upto` (i.e., the start of the live conversation that follows).
    upto: Seq,
    /// The one-line summary the model wrote at consolidation time.
    summary: String,
}

/// Pre-pass: collect boundary marks so the main loop can elide their spans.
///
/// Returns `(boundaries, elided_seqs)`:
/// - `boundaries` — one entry per [`MarkKind::Boundary`], in log order.
/// - `elided_seqs` — the set of [`Seq`]s to skip in the main loop; for each
///   boundary this is every entry with `seq <= upto` plus the mark itself.
///
/// When multiple boundaries exist the folds may overlap (each later
/// consolidation subsumes the earlier ones). The later boundary's summary
/// wins for any seq covered by both folds.
fn extract_boundaries(log: &[LogEntry]) -> (Vec<Boundary>, HashSet<Seq>) {
    let mut boundaries = Vec::new();
    let mut elided = HashSet::new();
    for entry in log {
        if let Payload::Mark(Mark {
            kind: MarkKind::Boundary { upto },
            note,
        }) = &entry.payload
        {
            let upto = *upto;
            // Elide the old-task span and the mark itself.
            for e in log {
                if e.seq <= upto || e.seq == entry.seq {
                    elided.insert(e.seq);
                }
            }
            boundaries.push(Boundary {
                upto,
                summary: note.clone(),
            });
        }
    }
    (boundaries, elided)
}

/// Where a tool result's images land on the wire (docs/log-and-projection.md).
/// Chosen once at boot from the provider and fixed for the session, so
/// projection stays deterministic: same log + same placement → same bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImagePlacement {
    /// Image blocks inside the `tool_result` content array — Anthropic's
    /// documented shape, verified end-to-end against the native API.
    #[default]
    InToolResult,
    /// The `tool_result` stays text-only; each image follows it as a top-level
    /// `image` block in the same user message. The portable shape for bridged
    /// providers whose translation drops `tool_result` images: OpenRouter's
    /// Anthropic skin documents images as user content only, and its Gemini
    /// bridge 400s on `tool_result` images (docs/transport.md).
    FollowingUserBlock,
    /// The `tool_result` stays text-only and each image rides a *separate* user
    /// message, preceded by a one-line assistant turn naming the result the
    /// images answer.
    ///
    /// Gemini rejects a replayed thought signature whenever an image sits
    /// between the signed `tool_use` and the next assistant turn — 400
    /// "Corrupted thought signature" (docs/transport.md). Placement *within* the
    /// function-response turn does not move it: image-in-`tool_result`,
    /// image-as-following-block, and a bare second user message all fail
    /// identically, and the failure is the same on the provider's OpenAI-shaped
    /// endpoint, so it is not the Anthropic skin's translation. An interposed
    /// assistant turn is the only shape that clears it, which is why this arm
    /// writes wire content no log entry produced.
    AfterInterposedTurn,
}

/// [`project_with`] at the default (Anthropic-native) image placement.
pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {
    project_with(log, ImagePlacement::default())
}

/// Fold the append-only log into the wire transcript.
///
/// Deterministic by construction: a pure function of `log` and `placement`,
/// with no clock, map iteration, or other ambient input leaking into the
/// output.
///
/// When the log contains a [`MarkKind::Boundary`], the span it names (`seq <=
/// upto`) is elided and replaced by a single user-role summary message injected
/// at the mark's position. The live conversation after `upto` — and the
/// consolidate `tool_use` / result that recorded the boundary — project normally
/// (docs/README.md).
pub fn project_with(log: &[LogEntry], placement: ImagePlacement) -> Vec<ApiMessage> {
    encode_messages(
        &project_semantic(log),
        placement,
        BreakpointPlacement::default(),
    )
}

pub(crate) fn messages_block_to_api(block: &MessagesOutputBlock) -> ApiBlock {
    match block {
        MessagesOutputBlock::Text(text) => ApiBlock::Text {
            text: text.clone(),
            cache_control: None,
        },
        MessagesOutputBlock::Thinking { text, signature } => ApiBlock::Thinking {
            thinking: text.clone(),
            signature: signature.clone(),
            cache_control: None,
        },
        MessagesOutputBlock::RedactedThinking { data } => ApiBlock::RedactedThinking {
            data: data.clone(),
            cache_control: None,
        },
        MessagesOutputBlock::ToolUse(call) => ApiBlock::ToolUse {
            id: call.id.0.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            cache_control: None,
        },
    }
}

pub const PEEK_LINE_CAP: usize = 2000;

#[cfg(test)]
mod test;

mod messages;
mod render;
mod retention;
mod semantic;
mod wire;

pub use messages::encode_messages;
pub use semantic::{
    DialectConflict, UserPart, WireTurn, check_dialect, project_semantic, recorded_dialect,
    render_result,
};

pub use render::{Distillate, distillate, render_distilled, render_triage_failure};
pub use retention::Retention;
pub use wire::{
    ApiBlock, ApiMessage, BreakpointPlacement, CacheControl, ImageSource, ResultBlock, Role,
    ToolResultContent, place_trailing_cache_breakpoint,
};
