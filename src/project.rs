//! `project(log) -> Vec<ApiMessage>` — the deterministic view sent to the API
//! (docs/README.md, docs/log-and-projection.md).
//!
//! The whole thesis rides on this being **byte-stable**: identical input must
//! serialize to identical bytes every time, or prompt caching breaks silently.
//! The hazards (docs/log-and-projection.md) are non-deterministic map ordering (avoided by
//! using [`serde_json::Value`], which serializes object keys in sorted order) and
//! any accidental change to field order / naming on the wire (pinned by the
//! expected-bytes test below).

use serde::Serialize;
use serde_json::Value;

use std::collections::{HashMap, HashSet};

use crate::context::Item;
use crate::log::{
    CallId, ContentBlock, Disposition, Keep, LogEntry, Mark, MarkKind, Notification, Payload,
    RawResult, Seq, WireState,
};

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

/// Fold the append-only log into the wire transcript.
///
/// Deterministic by construction: a pure function of `log`, with no clock, map
/// iteration, or other ambient input leaking into the output.
///
/// When the log contains a [`MarkKind::Boundary`], the span it names (`seq <=
/// upto`) is elided and replaced by a single user-role summary message injected
/// at the mark's position. The live conversation after `upto` — and the
/// consolidate `tool_use` / result that recorded the boundary — project normally
/// (docs/README.md).
pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {
    let (boundaries, elided) = extract_boundaries(log);
    // Notification bodies by arrival `seq`, so a `Triage` can emit a block for each
    // notification it pinned (docs/inbox.md) without re-walking the log.
    let notifications: HashMap<Seq, &Notification> = log
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Ambient(n) => Some((e.seq, n)),
            _ => None,
        })
        .collect();
    // The `[context]` listing by item name — (description, path) for each read-on-demand
    // stub — so a `Triage` can re-render a surfaced context item (docs/redaction-fork.md)
    // at cache-read cost, mirroring the notification body map. Sourced from the last boot
    // `Preamble`'s structured snapshot (context.md); only looked up by name, so `HashMap`
    // order can't reach the wire.
    let listing: HashMap<&str, (&str, &str)> = log
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Preamble { context, .. } => Some(context),
            _ => None,
        })
        .into_iter()
        .flat_map(|ctx| ctx.sections.iter().flat_map(|s| &s.items))
        .filter_map(|item| match item {
            Item::Listed {
                name,
                description,
                path,
            } => Some((name.as_str(), (description.as_str(), path.as_str()))),
            Item::Inline { .. } | Item::Exec { .. } => None,
        })
        .collect();
    // Cursor into `boundaries` — advance past each one as we emit the first
    // non-elided entry after its `upto`.
    let mut boundary_cursor = 0;

    let mut messages = Vec::new();
    // The content of the user message currently being assembled. `tool_result`
    // blocks coalesce here (Anthropic groups the results answering one assistant
    // turn's `tool_use`s into the following user turn), and a `User` entry that
    // immediately follows them merges into the *same* message rather than forming a
    // second consecutive user-role message — the seam-drain projection shape
    // (docs/log-and-projection.md): an injected/redirect user turn appended
    // right after a turn's `tool_result`s must project as one user message
    // `[tool_result…, text]`, preserving role-alternation and byte-stability. We
    // accumulate and flush at the next assistant / boundary / end.
    let mut pending_user_content: Vec<ApiBlock> = Vec::new();

    for entry in log {
        // Inject the summary just before the first non-elided entry that follows
        // the fold span (i.e., the first entry with seq > upto). This places the
        // summary at the head of the live conversation rather than at the mark's
        // own log position (docs/README.md).
        while boundary_cursor < boundaries.len()
            && !elided.contains(&entry.seq)
            && entry.seq > boundaries[boundary_cursor].upto
        {
            let b = &boundaries[boundary_cursor];
            flush_pending_user(&mut pending_user_content, &mut messages);
            messages.push(ApiMessage {
                role: Role::User,
                content: vec![ApiBlock::Text {
                    text: b.summary.clone(),
                    cache_control: None,
                }],
            });
            boundary_cursor += 1;
        }

        // Skip elided entries (the folded old-task span + the mark itself).
        if elided.contains(&entry.seq) {
            continue;
        }

        match &entry.payload {
            Payload::User(blocks) => {
                // Merge into the pending user message rather than flushing first: a
                // user turn following this turn's `tool_result`s lands as one message
                // `[tool_result…, text]` (docs/log-and-projection.md). With
                // nothing pending this is just a fresh user message, flushed at the
                // next assistant / boundary / end — byte-identical to a direct push.
                pending_user_content.extend(blocks.iter().map(block_to_api));
            }
            Payload::Assistant {
                thinking,
                text,
                calls,
            } => {
                flush_pending_user(&mut pending_user_content, &mut messages);
                // Wire order: thinking → text → tool_use (docs/transport.md). Thinking blocks
                // lead so the replayed signatures sit exactly where the API expects them.
                let mut content: Vec<ApiBlock> = thinking
                    .iter()
                    .chain(text.iter())
                    .map(block_to_api)
                    .collect();
                for call in calls {
                    content.push(ApiBlock::ToolUse {
                        id: call.id.0.clone(),
                        name: call.name.clone(),
                        input: call.input.clone(),
                        cache_control: None,
                    });
                }
                messages.push(ApiMessage {
                    role: Role::Assistant,
                    content,
                });
            }
            Payload::ToolResult { call_id, raw, wire } => {
                pending_user_content.push(tool_result_block(entry.seq, call_id, raw, wire))
            }
            // Marks are in the elided set (extract_boundaries inserts every
            // Boundary mark's own seq); this arm is only reachable for future
            // non-Boundary mark kinds, which can be added here when they land.
            Payload::Mark(_) => {}
            // An `Ambient` arrival is audit-only on the wire (docs/inbox.md) — it never
            // itself projects; a *pinned* one is emitted by its `Triage` (below).
            Payload::Ambient(_) => {}
            // A spotter verdict (docs/permissions.md) is audit-only — the permission
            // decision lives in the log, never on the canonical wire.
            Payload::Spot(_) => {}
            // A grant (docs/permissions.md) is audit-only too — its
            // *effect* reaches the model through the classifier (the policy fold), never
            // the transcript. So it projects to nothing, and appending one cannot perturb
            // the cached prefix (docs/log-and-projection.md).
            Payload::ConfigEdit(_) => {}
            // The boot preamble snapshot (docs/context.md) is replay/audit data:
            // the system prompt rides the request's `system` field, never the `messages`
            // array, so the snapshot projects to nothing.
            Payload::Preamble { .. } => {}
            // A fork verdict emits a block per **pinned** notification and per **surfaced**
            // context item at its own position — notification bodies looked up from the
            // `Ambient` arrival (docs/inbox.md), surfaced items re-rendered from the boot
            // listing (docs/redaction-fork.md). This tail-ward placement keeps the wire
            // append-only: content acknowledged at a later gate than its origin never splices
            // behind a breakpoint (docs/log-and-projection.md). Dismissed / left / already-surfaced
            // dispositions don't project. The verdict is otherwise audit-only.
            Payload::Triage(t) => {
                let mut blocks: Vec<String> = t
                    .inbox
                    .iter()
                    .filter(|(_, d)| matches!(d, Disposition::Pin))
                    .filter_map(|(seq, _)| {
                        notifications
                            .get(seq)
                            .map(|n| format!("⟢ {}: {}", n.source, n.body))
                    })
                    .collect();
                // Surfaced context items — name resolved to its one-line description and
                // read path off the listing. A name absent from the current listing (a
                // cross-boot listing change) simply drops: deterministic, never a panic.
                blocks.extend(t.surfaced.iter().filter_map(|name| {
                    listing.get(name.as_str()).map(|(description, path)| {
                        format!("⟢ context · {name} may be relevant: {description} — read {path}")
                    })
                }));
                if !blocks.is_empty() {
                    flush_pending_user(&mut pending_user_content, &mut messages);
                    messages.push(ApiMessage {
                        role: Role::User,
                        content: vec![ApiBlock::Text {
                            text: blocks.join("\n"),
                            cache_control: None,
                        }],
                    });
                }
            }
        }
    }

    // Flush any boundaries whose upto is past the end of the log (degenerate
    // edge: consolidate called with nothing after it yet).

    while boundary_cursor < boundaries.len() {
        let b = &boundaries[boundary_cursor];
        flush_pending_user(&mut pending_user_content, &mut messages);
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![ApiBlock::Text {
                text: b.summary.clone(),
                cache_control: None,
            }],
        });
        boundary_cursor += 1;
    }

    flush_pending_user(&mut pending_user_content, &mut messages);
    messages
}

fn flush_pending_user(pending: &mut Vec<ApiBlock>, messages: &mut Vec<ApiMessage>) {
    if !pending.is_empty() {
        messages.push(ApiMessage {
            role: Role::User,
            content: std::mem::take(pending),
        });
    }
}

fn block_to_api(block: &ContentBlock) -> ApiBlock {
    match block {
        ContentBlock::Text(text) => ApiBlock::Text {
            text: text.clone(),
            cache_control: None,
        },
        ContentBlock::Thinking { text, signature } => ApiBlock::Thinking {
            thinking: text.clone(),
            signature: signature.clone(),
            cache_control: None,
        },
        ContentBlock::RedactedThinking { data } => ApiBlock::RedactedThinking {
            data: data.clone(),
            cache_control: None,
        },
    }
}

fn tool_result_block(seq: u64, call_id: &CallId, raw: &RawResult, wire: &WireState) -> ApiBlock {
    let (content, is_error) = match wire {
        WireState::Raw => (raw.content.clone(), raw.is_error),
        // Re-render the distillate from `raw` + the stored verdict each turn
        // (docs/redaction-fork.md) — the kept bytes are never persisted twice.
        WireState::Distilled { keep, note } => (
            render_distilled(&raw.content, keep, note.as_deref()),
            raw.is_error,
        ),
        // Project a placeholder rather than nothing: dropping the block entirely
        // would orphan the assistant `tool_use` it answers, which the API
        // rejects. Constructed by the interrupt (`withheld` in `src/agent.rs`,
        // docs/control-channel.md), which is also the answer to the question
        // flagged here before it had one — elision keeps the pair; the placeholder
        // exists precisely so the `tool_use` stays answered.
        WireState::Elided => ("[elided]".to_string(), false),
    };
    // Prefix with [seq] handle so tool results are describable as 'reread 14' /
    // 'branch from 14' — the entry's stable seq. The wire never carries seq
    // itself; this handle makes it visible at read-time without schema change
    // (docs/README.md).
    let prefixed = format!("[{}] {}", seq, content);
    ApiBlock::ToolResult {
        tool_use_id: call_id.0.clone(),
        content: prefixed,
        is_error,
        cache_control: None,
    }
}

/// Hard cap on the lines the fork peek shows the model (docs/redaction-fork.md). The canonical
/// render interprets a verdict's ranges against this same prefix of `raw`, so the two
/// stay aligned — the model's `retain(lines=…)` coordinates address exactly what it saw.
pub const PEEK_LINE_CAP: usize = 2000;

#[cfg(test)]
mod test;

mod render;
mod wire;

pub use render::{Distillate, distillate, render_distilled};
pub use wire::{ApiBlock, ApiMessage, CacheControl, Role, place_trailing_cache_breakpoint};
