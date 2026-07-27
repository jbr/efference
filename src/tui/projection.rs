//! Project the append-only log (plus the in-flight streaming tail) into a
//! renderable [`Document`] (docs/README.md — the transcript is another `project(log)`,
//! a sibling of the wire projection in `crate::project`).
//!
//! This mirrors two rules from the wire projection ([`crate::project`]): a tool
//! result shows its *distillate* when present, else its raw content
//! ([`tool_block`]); and long output is collapsible (here via a
//! [`DocumentNode::TruncatedBlock`] rather than the wire's `[seq]` addressing).
//! Unlike the wire projection it does not coalesce or elide — every turn renders,
//! so the human sees the whole record.

use std::collections::{HashMap, HashSet};

use crate::log::{ContentBlock, LogEntry, MarkKind, Payload, RawResult, ToolCall, WireState};
use crate::tui::document::{
    Author, DiffLine, DiffLineKind, Document, DocumentNode, ListItem, Span, SpanStyle,
    TruncationLevel,
};
use crate::tui::markdown::MarkdownRenderer;

/// Fold the log + live tail into a transcript document of conversational
/// [`Turn`](DocumentNode::Turn)s. Authorship is structural (the renderer draws a
/// gutter for the user; the assistant is unmarked), and each tool call is paired
/// with its result into one collapsible *exchange* (`❯ tool · arg` over the
/// result) rather than two separately-labeled blocks. `view` carries the human's
/// per-exchange toggles (expand, raw-vs-wire) across this rebuild (docs/tui.md).
///
/// `pending_call` is the `call_id` the agent is *blocked on for approval* (docs/tui.md).
/// The loop appends the assistant `tool_use` **before** it gates (so the
/// call is already in the log), and one tool call runs per turn (docs/README.md) — so the
/// pending call is always the tail result-less exchange. That exchange renders
/// *as if invoked*, with a `⏸` marker and an inline affordance where the result
/// will land, so the gate reads as "after the call, before the result" rather than
/// a banner divorced from the call it's gating.
///
/// `pending_turns` are just-submitted user messages not yet echoed back as committed
/// `User` log entries (docs/tui.md). They render **after** the
/// committed log and live tail, dimmed with a `⋯` marker — the optimistic "handed to the
/// daemon, not yet on the wire" echo — and are dropped as their matching `User` entry
/// arrives via `/events`.
pub(crate) fn project(
    log: &[LogEntry],
    live: &LiveTail,
    view: &ViewState,
    pending_call: Option<&str>,
    pending_reason: Option<&str>,
    pending_turns: &[String],
) -> Document<'static> {
    // A result lands in a later entry than the call it answers; index results by
    // `call_id` up front so a call can be paired with its result in one place.
    let mut result_for_call: HashMap<&str, (&RawResult, &WireState)> = HashMap::new();
    for entry in log {
        if let Payload::ToolResult { call_id, raw, wire } = &entry.payload {
            result_for_call.insert(call_id.0.as_str(), (raw, wire));
        }
    }

    let mut nodes: Vec<DocumentNode<'static>> = Vec::new();
    for entry in log {
        match &entry.payload {
            Payload::User(blocks) => {
                nodes.push(turn(Author::User, markdown(&join_text(blocks))));
            }
            Payload::Assistant {
                thinking,
                text,
                calls,
            } => {
                let mut blocks = Vec::new();
                // Thinking leads the turn, as it does on the wire (`thinking` → `text` →
                // `tool_use`). It is the *record* of the reasoning: watching it stream past at
                // generation speed is not reading it, so the transcript has to keep it.
                if let Some(block) = thinking_block(entry.seq, thinking, view) {
                    blocks.push(block);
                }
                let prose = join_text(text);
                if !prose.trim().is_empty() {
                    blocks.extend(markdown(&prose));
                }
                for call in calls {
                    let result = result_for_call.get(call.id.0.as_str()).copied();
                    let is_pending = result.is_none() && pending_call == Some(call.id.0.as_str());
                    blocks.push(exchange(call, result, view));
                    // Harness-generated structure about a call is a **sibling** of the
                    // exchange, never a child of it (docs/tui.md). The exchange's
                    // collapsible body holds only what the agent produced — the call's
                    // arguments and its result — so the body's truncation can never
                    // reach an annotation the harness added. Nesting these was how a
                    // multi-line command clipped away the whole gate.
                    //
                    // Reading order is call → why it is held → what to do, so the
                    // affordance lands last, adjacent to the strip carrying the verbs.
                    if is_pending {
                        if let Some(reason) = pending_reason {
                            blocks.push(spotter_reason_line(reason));
                        }
                        blocks.push(gate_affordance());
                    }
                }
                if !blocks.is_empty() {
                    nodes.push(turn(Author::Assistant, blocks));
                }
            }
            // Folded into its call's exchange above.
            Payload::ToolResult { .. } => {}
            Payload::Mark(mark) => push_mark(&mut nodes, &mark.kind, &mark.note),
            // An ambient notification (docs/inbox.md), rendered as a marked line so the
            // human sees the out-of-band event in the stream. The transcript shows the
            // whole record — every *shown* notification renders here, where the wire
            // carries only the pinned ones.
            Payload::Ambient(n) => nodes.push(DocumentNode::Paragraph {
                spans: vec![
                    Span {
                        text: format!("⟢ {} · ", n.source).into(),
                        style: SpanStyle::Comment,
                        action: None,
                    },
                    Span::plain(n.body.clone()),
                ],
            }),
            // The verdict is internal — audit-only on the transcript (docs/redaction-fork.md).
            Payload::Triage(_) => {}
            // A spotter permission verdict (docs/permissions.md) — audit-only, like Triage.
            Payload::Spot(_) => {}
            // A grant (docs/permissions.md) — audit-only on the transcript
            // for now; surfacing scope changes to the human lands with the construction sites.
            Payload::ConfigEdit(_) => {}
            // The boot preamble snapshot (docs/context.md) — session boot record,
            // not conversation; the human inspects it via `efference wire`, not the transcript.
            Payload::Preamble { .. } => {}
        }
    }

    // The live tail renders after everything committed: in-flight assistant prose
    // as its own turn, and each running tool as a result-less exchange.
    if !live.assistant.is_empty() {
        nodes.push(turn(Author::Assistant, markdown(&live.assistant)));
    }
    // A generation phase that produces no prose (thinking, or a tool call's input still
    // arriving). Without this the window between "turn submitted" and "first prose" is silent,
    // and silence is indistinguishable from a hung harness (backlog/dogfooding-friction.md).
    if let Some(phase) = &live.phase {
        nodes.push(turn(Author::Assistant, phase_blocks(phase)));
    }
    for (call_id, buf) in &live.tools {
        let header = vec![inline_code(call_id.clone())];
        nodes.push(turn(
            Author::Assistant,
            vec![DocumentNode::collapsible(
                header,
                Some(prompt_span(false)),
                vec![code_block(None, buf)],
                TruncationLevel::Brief,
            )],
        ));
    }

    // Optimistic echoes of just-submitted turns, last of all (docs/tui.md).
    for text in pending_turns {
        nodes.push(pending_turn(text));
    }

    Document::with_nodes(nodes)
}

/// One tool call paired with its result as a collapsible exchange: the header is
/// the call (`❯ tool · arg`, or `✗ tool …` when the result errored); the body is
/// the (tool-specifically rendered) result. No `result #N` label — the result is
/// obviously the response to the call directly above it.
///
/// Agent-generated content only. Whether the call is *pending* is not an input here:
/// the gate is the harness talking about the call, so it renders as a sibling at the
/// call site rather than inside this body (docs/tui.md). A pending exchange is simply
/// one with no result yet — its body is the call's arguments and nothing else.
fn exchange(
    call: &ToolCall,
    result: Option<(&RawResult, &WireState)>,
    view: &ViewState,
) -> DocumentNode<'static> {
    let is_error = result.is_some_and(|(raw, _)| raw.is_error);
    let call_id = call.id.0.as_str();

    let mut header = vec![inline_code_styled(
        call.name.clone(),
        SpanStyle::FunctionName,
    )];
    let (inline, overflow) = call_args(call);
    if let Some(arg_spans) = inline {
        header.push(Span {
            text: " · ".into(),
            style: SpanStyle::Comment,
            action: None,
        });
        header.extend(arg_spans);
    }

    let mut body = overflow;
    if let Some((raw, wire)) = result {
        // The human can flip a distilled result to its full raw output (docs/tui.md)
        // — both are already client-side, so this is a pure render choice. A
        // forced-raw view passes a synthetic `Raw` wire so `result_body` gives it the
        // same rich tool-specific rendering (diff/read) a natural raw result gets.
        let show_raw = view.is_raw(call_id);
        let overriding = show_raw && !matches!(wire, WireState::Raw);
        let (content, effective_wire) = if show_raw {
            (raw.content.clone(), &WireState::Raw)
        } else {
            let content = match wire {
                WireState::Raw => raw.content.clone(),
                // Same renderer as the wire projection — re-derive the distillate from
                // `raw` + the stored verdict (docs/redaction-fork.md).
                WireState::Distilled { keep, note } => {
                    crate::project::render_distilled(&raw.content, keep, note.as_deref())
                }
                WireState::Elided => "[elided]".to_string(),
            };
            (content, wire)
        };
        // Flag that the leaner distilled default is being overridden, so the human
        // sees they're on raw (`r` to flip back).
        if overriding {
            header.push(Span {
                text: " · raw".into(),
                style: SpanStyle::Comment,
                action: None,
            });
        }
        body.extend(result_body(
            Some(call.name.as_str()),
            raw,
            effective_wire,
            &content,
        ));
    }

    // The resting marker is the `❯`/`✗` prompt — the renderer swaps in a ▶/▼
    // disclosure triangle when the body is long enough to collapse, which is exactly
    // why the pending `⏸` is *not* here: a marker that a long call replaces is not a
    // marker. It rides the gate affordance instead, which is a sibling and always
    // shown (docs/tui.md). The block is keyed by `call_id` so its expand/raw state
    // survives re-projection.
    let resting = prompt_span(is_error);
    let level = if view.is_expanded(call_id) {
        TruncationLevel::Full
    } else {
        TruncationLevel::Brief
    };
    DocumentNode::collapsible_keyed(call_id.to_string(), header, Some(resting), body, level)
}

/// Whether a block `key` names a tool call rather than a turn's thinking. The
/// distinction is what the block menu ranks on: only a tool result has a
/// distillate/raw fork to flip (docs/redaction-fork.md).
pub(super) fn is_call_key(key: &str) -> bool {
    !key.starts_with(THINKING_KEY_PREFIX)
}

#[cfg(test)]
mod test;

mod nodes;
mod tail;
mod tools;

pub(crate) use nodes::join_text;
pub(crate) use tail::{LiveTail, Phase, ViewState};

use nodes::*;
use tools::*;
