//! The JSON tool schemas the fork offers the distilling model (docs/redaction-fork.md).

use super::*;

/// The `inbox` triage schema field (seq-keyed pin/dismiss) — shared by `retain` and
/// the standalone `triage` tool.
pub(super) fn inbox_schema_field() -> Value {
    json!({
        "type": "object",
        "description": "Triage your pending notifications by [id]. Unlisted ids are left for the next decision point.",
        "properties": {
            "pin": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "Notification ids to pin into your working context going forward."
            },
            "dismiss": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "Notification ids to clear as noise."
            }
        }
    })
}

/// The `background` triage schema field (handle-keyed pin/mute) — shared by `retain`
/// and the standalone `triage` tool.
pub(super) fn background_schema_field() -> Value {
    json!({
        "type": "object",
        "description": "Triage live background processes by [bg#n] handle. Unlisted handles keep streaming their tail at the next decision point.",
        "properties": {
            "pin": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Handles whose current tail to capture onto the record now (e.g. \"bg#3\")."
            },
            "mute": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Handles whose running tail to stop showing — they keep running and still notify on exit."
            }
        }
    })
}

/// The `surfaced` schema field (name-keyed) — present only when the roster is non-empty.
/// A plain string array: names are validated against the roster in code (invented ones
/// dropped), not by a schema enum, keeping the guard harness-side (docs/redaction-fork.md).
pub(super) fn surfaced_schema_field() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Names of available context items (listed under \"context\" above) that genuinely bear on what you're doing now — each is surfaced as a reminder to read it. Omit or leave empty if none apply."
    })
}

/// The forced fork tool. Its `input_schema` gains an `inbox` field only when there are
/// notifications to triage, a `background` field only when processes are running, and a
/// `surfaced` field only when there are unloaded context items to offer — each absent case
/// stays lean (and the model isn't prompted to invent dispositions).
pub(super) fn retain_tool(
    shown: &[ShownNotification],
    background: &[BgView],
    roster: &[String],
) -> Value {
    let mut input_schema = json!({
        "type": "object",
        "properties": {
            "keep": {
                "type": "string",
                "description": "\"all\", \"none\", or comma-separated 1-based inclusive line ranges, e.g. \"40-52,310-318\" (a single line is \"7\")."
            },
            "note": {
                "type": "string",
                "description": "One-line summary of what the output showed; the breadcrumb kept when little or nothing is retained."
            }
        },
        "required": ["keep"]
    });
    if !shown.is_empty() {
        input_schema["properties"]["inbox"] = inbox_schema_field();
    }
    if !background.is_empty() {
        input_schema["properties"]["background"] = background_schema_field();
    }
    if !roster.is_empty() {
        input_schema["properties"]["surfaced"] = surfaced_schema_field();
    }
    json!({
        "name": RETAIN,
        "description": "Choose what of the tool output to keep on the record going forward. The output is line-numbered. Whatever you drop is gone from the working context, so keep anything that might matter — but stay lean.",
        "input_schema": input_schema
    })
}

/// The forced tool of the standalone idle-triage peek (docs/supervisor.md, docs/inbox.md):
/// inbox/background dispositions only, no `keep` (there's no tool output). At least one
/// section is always present (the loop skips the peek when both are empty).
pub(super) fn triage_tool(shown: &[ShownNotification], background: &[BgView]) -> Value {
    let mut properties = serde_json::Map::new();
    if !shown.is_empty() {
        properties.insert("inbox".into(), inbox_schema_field());
    }
    if !background.is_empty() {
        properties.insert("background".into(), background_schema_field());
    }
    json!({
        "name": TRIAGE,
        "description": "Triage your pending notifications and running background processes. Pin what needs to enter your working context and be acted on now; dismiss noise; leave anything you're deferring unlisted (pinning nothing returns you to idle, unchanged).",
        "input_schema": { "type": "object", "properties": properties }
    })
}
