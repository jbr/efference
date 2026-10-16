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

use crate::log::{
    Audience, Keep, LogEntry, MadeBy, MessagesOutputBlock, Payload, Seq, Summary, Triaged,
};

/// Which client a projection feeds (docs/summary.md). `Main` is the agent's own
/// wire, and the one a triage that shares the agent's client sends; `Triage` is a
/// triage model running on its own client. The two differ only in which summaries
/// apply: a summary whose [`Audience`] is `Triage` replaces its range for `Triage` and is
/// absent from `Main`, so the main wire's bytes do not depend on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reader {
    #[default]
    Main,
    Triage,
}

impl Reader {
    pub(crate) fn applies(self, audience: Audience) -> bool {
        match audience {
            Audience::All => true,
            Audience::Triage => self == Reader::Triage,
        }
    }
}

/// A summary and where it lands, extracted in the pre-pass.
struct Boundary {
    /// The last seq of the summary's inclusive replacement range.
    /// The text is injected just before the first non-elided entry with
    /// `seq > upto` (i.e., the start of the live conversation that follows).
    upto: Seq,
    /// The text as projected, with its header when the agent wrote it.
    summary: String,
}

/// The text a summary contributes. A consolidation is introduced as a harness note
/// so it is not read as the operator's turn; a compaction is projected as written,
/// since its header carries the explanation (docs/consolidate.md, docs/summary.md).
fn account(summary: &Summary) -> String {
    let Summary {
        from, upto, text, ..
    } = summary;
    match summary.made_by {
        MadeBy::Compaction(_) => text.clone(),
        MadeBy::Consolidation => format!(
            "⟢ consolidation: entries #{from}–#{upto} were replaced by this account of \
             them, written by this session with those entries in view.\n\n{text}"
        ),
    }
}

/// Collect applicable head/span accounts and the union of their elided ranges
/// (docs/consolidate.md). Marks themselves never project at their append position.
fn extract_boundaries(
    log: &[LogEntry],
    skipped: &HashSet<Seq>,
    reader: Reader,
) -> (Vec<Boundary>, HashSet<Seq>) {
    let mut boundaries = Vec::new();
    let mut elided = HashSet::new();
    for entry in log {
        // A mark the session rolled back never took effect.
        if skipped.contains(&entry.seq) {
            continue;
        }
        if let Payload::Summary(summary) = &entry.payload {
            // A summary for the other reader projects to nothing here.
            if !reader.applies(summary.audience) {
                continue;
            }
            let range = summary.range();
            let upto = summary.upto;
            // Elide the named range and the summary entry itself.
            for e in log {
                if range.contains(&e.seq) || e.seq == entry.seq {
                    elided.insert(e.seq);
                }
            }
            boundaries.push(Boundary {
                upto,
                summary: account(summary),
            });
        }
    }
    (boundaries, elided)
}

/// The seqs every `Resumed` entry rolled back (docs/resumption.md): those after its
/// `resumes_after` and before its own seq. Empty for a log with no skipped range, which
/// is what leaves such a log's projection untouched.
pub(crate) fn skipped_seqs(log: &[LogEntry]) -> HashSet<Seq> {
    log.iter()
        .filter_map(|entry| match &entry.payload {
            Payload::Resumed(resumed) => Some(resumed.skipped(entry.seq)),
            _ => None,
        })
        .flatten()
        .collect()
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
/// When the log contains a [`Summary`], the range it names (`seq <=
/// upto`) is elided and replaced by a single user-role summary message injected
/// at the summary's position. The live conversation after `upto` — and the
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

pub const TRIAGE_LINE_CAP: usize = 2000;

#[cfg(test)]
mod test;

mod messages;
mod render;
mod retention;
mod semantic;
mod wire;

pub use messages::encode_messages;
pub use semantic::{
    DialectConflict, UserPart, WireTurn, check_dialect, project_semantic, project_semantic_for,
    recorded_dialect, render_result,
};

pub use render::{
    INTENT_FEEDBACK_LEAD, Selection, render_resumed, render_triage_failure, render_triaged,
    selection,
};
pub use retention::Retention;
pub use wire::{
    ApiBlock, ApiMessage, BreakpointPlacement, CacheControl, ImageSource, ResultBlock, Role,
    ToolResultContent, place_trailing_cache_breakpoint,
};
