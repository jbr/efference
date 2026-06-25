//! The `consolidate` tool (DESIGN §10 Phase 3) — the agent's own memory-
//! consolidation primitive. The agent calls it to fold the prior task's
//! trajectory into a concise summary, so the working context stays lean as a
//! session runs long.
//!
//! **Slice 1 implemented:** the boundary [`Mark`] now folds on the wire.
//! `project` elides every entry with `seq <= upto` and injects the summary as
//! a user-role text message just before the first live entry that follows the
//! fold. The consolidate `tool_use` / `tool_result` pair that records the
//! boundary projects normally (visible on the wire as low-noise context).
//! The raw transcript survives in the log for reversibility.
//!
//! It is the first tool that acts on the **log**, not the world: it produces a
//! `Mark`, not a [`RawResult`] to distill, so the loop handles it structurally
//! (`agent.rs`) rather than through the world-facing [`Toolbox`].
//!
//! [`Mark`]: crate::log::Mark
//! [`RawResult`]: crate::log::RawResult
//! [`Toolbox`]: crate::tools::Toolbox

use serde::Deserialize;
use serde_json::{Value, json};

use crate::log::{LogEntry, Payload, Seq};

/// The wire name. The loop branches on this to take the structural path.
pub const NAME: &str = "consolidate";

/// The `tools[]` entry advertised alongside the world-facing tools.
///
/// Framed per the **"distillate IS the output"** lesson (DESIGN §4-realized): the
/// summary *becomes* the record — deliberately no "you can re-read it later", which
/// manufactured anxiety and over-dropping when the peek believed its work was
/// recoverable. The raw transcript does survive in the log, but that is an
/// operator/audit property, not something the prompt dangles.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "description": "Consolidate the work you've finished into a concise summary that replaces its detailed transcript in your working context. Best done right as you start a new task: capture what you accomplished and the facts worth carrying forward — interfaces you added, where useful functions live, decisions you made — and write it as you'd want to read it at the top of the next task. This summary becomes the record of that work going forward.",
        "input_schema": {
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "The concise summary of the finished work that replaces its transcript."
                }
            },
            "required": ["summary"]
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

/// The `upto` a boundary placed now folds: everything up to the entry just before
/// the **live task instruction** — the most recent user turn, which must survive so
/// the agent keeps the fresh ask. Append-only means the boundary mark sits at the
/// tail, so it names this extent explicitly (DESIGN §10 → projection fold). The
/// field is recorded now and read by the Slice-1 fold.
///
/// Degenerate fallback when there's no earlier entry to fold to (e.g. a consolidate
/// with no later user turn): the live user seq itself. Slice 1 refines the
/// self-directed edge with the fold + its tests.
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
mod tests {
    use super::*;
    use crate::log::{ContentBlock, LogEntry, Payload, Timestamp};

    fn at(seq: Seq, payload: Payload) -> LogEntry {
        LogEntry {
            seq,
            turn: 0,
            at: Timestamp(seq),
            payload,
        }
    }
    fn user(seq: Seq) -> LogEntry {
        at(seq, Payload::User(vec![ContentBlock::Text("u".into())]))
    }
    fn work(seq: Seq) -> LogEntry {
        at(
            seq,
            Payload::Assistant {
                text: vec![],
                calls: vec![],
            },
        )
    }

    #[test]
    fn extent_folds_up_to_just_before_the_live_user_turn() {
        // task A (0..=2), the new task instruction at 3, then the consolidate turn.
        let log = vec![user(0), work(1), work(2), user(3), work(4)];
        assert_eq!(
            extent(&log),
            2,
            "fold task A; preserve the live instruction at seq 3"
        );
    }

    #[test]
    fn summary_parses_and_tolerates_garbage() {
        assert_eq!(
            summary(&json!({"summary": "did X"})).as_deref(),
            Some("did X")
        );
        assert!(summary(&json!({"nope": 1})).is_none());
    }

    #[test]
    fn definition_advertises_the_name_and_a_required_summary() {
        let d = definition();
        assert_eq!(d["name"], json!(NAME));
        assert_eq!(d["input_schema"]["required"], json!(["summary"]));
    }
}
