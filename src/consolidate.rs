//! The `consolidate` tool (docs/consolidate.md) — the agent's own memory-
//! consolidation primitive. The agent calls it to fold the prior task's
//! trajectory into a concise summary, so the working context stays lean as a
//! session runs long. The raw transcript survives in the log for reversibility.
//!
//! It acts on the **log**, not the world: it produces a
//! `Mark`, not a [`RawResult`] to distill, so the loop handles it structurally
//! (`agent.rs`) rather than through the world-facing [`Toolbox`].
//!
//! [`Mark`]: crate::log::Mark
//! [`RawResult`]: crate::log::RawResult
//! [`Toolbox`]: crate::tools::Toolbox

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::log::{LogEntry, Payload, Seq};
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
/// Framed per the **"distillate IS the output"** lesson (docs/redaction-fork.md): the
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
         becomes the record of that work going forward."
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
/// (the loop falls back to an empty note rather than dropping the boundary).
pub fn summary(input: &Value) -> Option<String> {
    Input::deserialize(input).ok().map(|i| i.summary)
}

/// The `upto` a boundary folds: everything up to the entry just before the **live task
/// instruction** — the most recent user turn, which must survive so the agent keeps the
/// fresh ask. Append-only means the boundary mark sits at the tail, so it names this
/// extent explicitly (docs/consolidate.md).
///
/// Degenerate fallback when there's no earlier entry to fold to (e.g. a consolidate
/// with no later user turn): the live user seq itself.
pub fn extent(log: &[LogEntry]) -> Seq {
    match log
        .iter()
        .rev()
        .find(|e| matches!(e.payload, Payload::User(_)))
    {
        Some(live) => log
            .iter()
            .map(|e| e.seq)
            .filter(|&s| s < live.seq)
            .max()
            .unwrap_or(live.seq),
        None => log.last().map(|e| e.seq).unwrap_or(0),
    }
}

#[cfg(test)]
mod test;
