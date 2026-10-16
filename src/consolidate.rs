//! The `consolidate` tool (docs/consolidate.md) — the agent's own memory-
//! consolidation primitive. The agent calls it to fold the prior task's
//! trajectory into a concise summary, so the working context stays lean as a
//! session runs long. The raw transcript survives in the log for reversibility.
//!
//! It acts on the **log**, not the world: it produces a
//! `Mark`, not a [`RawResult`] to triage, so the loop handles it structurally
//! (`agent.rs`) rather than through the world-facing [`Toolbox`].
//!
//! [`Mark`]: crate::log::Mark
//! [`RawResult`]: crate::log::RawResult
//! [`Toolbox`]: crate::tools::Toolbox

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::log::{CallId, LogEntry, Payload, Seq};
use crate::tools::{ToolBrief, anthropic_def};
use mcplease::types::{Tool as ToolDefinition, ToolAnnotations};

/// The wire name. The loop branches on this to take the structural path.
pub const NAME: &str = "consolidate";

/// The mounted-table entry, in the same [`ToolDefinition`] shape a provider's
/// mount snapshots, so the spotter's brief (docs/permissions.md) reads this tool
/// like any other — the loop dispatches it structurally, outside the `Toolbox`,
/// which once left it brief-less at the rung. Session-local (closed-world) and
/// not a read; `destructive`/`idempotent` are left unstated — the fold is
/// reversible in the log but not on the wire, and neither honest answer fits a
/// one-bit hint.
///
/// Framed per the **"selection IS the output"** lesson (docs/triage.md): the
/// summary *becomes* the record — deliberately no "you can re-read it later", which
/// manufactured anxiety and over-dropping when the peek believed its work was
/// recoverable. The raw transcript does survive in the log, but that is an
/// operator/audit property, not something the prompt dangles.
static SCHEMA: LazyLock<ToolDefinition> = LazyLock::new(|| {
    let mut tool = ToolDefinition::new(
        NAME,
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "The concise summary of the finished work that replaces its transcript."
                }
            },
            "required": ["summary"]
        }),
    );
    tool.description = Some(
        "Consolidate the work you've finished into a concise summary that replaces its \
         detailed transcript in your working context. Best done right as you start a new \
         task: capture what you accomplished and the facts worth carrying forward — \
         interfaces you added, where useful functions live, decisions you made — and \
         write it as you'd want to read it at the top of the next task. This summary \
         becomes the record of that work going forward. Write at least 100 words. \
         It replaces the work since the previous consolidation, or since the session \
         began; earlier summaries stay as they are. Called as your first action after \
         an operator message, that message and the report or question it answers stay \
         verbatim. Called mid-task, the replaced range runs through your last tool \
         result and includes the instructions you are working under, so carry forward \
         the ones that still apply. Do not reconstruct unsupported exact code or \
         identifiers."
            .into(),
    );
    tool.annotations = Some(ToolAnnotations {
        read_only_hint: Some(false),
        open_world_hint: Some(false),
        ..Default::default()
    });
    tool
});

/// The `tools[]` entry advertised alongside the world-facing tools — derived from
/// the same [`SCHEMA`] the brief reads, through the same `anthropic_def` as a
/// provider's entries (sorted keys; annotations have no wire slot), so the bytes
/// stay stable for the session (docs/log-and-projection.md).
pub fn definition() -> Value {
    anthropic_def(&SCHEMA)
}

/// The spotter's brief for `consolidate` (docs/permissions.md); `None` for any
/// other name. `server: None` — the harness's own.
pub fn brief(name: &str) -> Option<ToolBrief<'static>> {
    (name == NAME).then(|| {
        let schema: &'static ToolDefinition = &SCHEMA;
        ToolBrief {
            server: None,
            definition: schema,
        }
    })
}

#[derive(Deserialize)]
struct Input {
    summary: String,
}

/// Pull the summary note out of a `consolidate` call's input; `None` if malformed
/// (the loop refuses a missing or short note).
pub fn summary(input: &Value) -> Option<String> {
    Input::deserialize(input).ok().map(|i| i.summary)
}

/// Calls split by a head-fold cut, used by the digest/narrator path
/// (docs/summary-head.md). Working-span checks additionally reject unanswered
/// calls and validate both range edges (docs/consolidate.md).
pub fn split_calls(log: &[LogEntry], upto: Seq) -> Vec<CallId> {
    // Which side of the boundary each call was made on, by id.
    let call_folded: HashMap<CallId, bool> = log
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Assistant {
                output,
                upstream_failure: None,
            } => Some((e.seq <= upto, output.tool_calls().unwrap_or_default())),
            _ => None,
        })
        .flat_map(|(folded, calls)| calls.into_iter().map(move |c| (c.id, folded)))
        .collect();
    let mut split = Vec::new();
    for entry in log {
        let Payload::ToolResult { call_id, .. } = &entry.payload else {
            continue;
        };
        if call_folded.get(call_id) == Some(&(entry.seq > upto)) && !split.contains(call_id) {
            split.push(call_id.clone());
        }
    }
    split
}

#[cfg(test)]
mod test;

mod span;
pub use span::{Span, before, candidate, safe, tool_bytes};
