//! The JSON tool schemas of the triage tools (docs/triage.md).
//!
//! Both schemas are **static** — zero-argument builders, every field unconditional —
//! because they ride the agent turn's `tools[]` (docs/triage.md): a schema that
//! varied with the inbox, the background set, or the roster would re-key the request
//! envelope mid-session and split the cache lineage, which is the exact defect the
//! shared array exists to close. Field descriptions are self-contained ("if a section
//! named X appears above…") since their referent sections aren't always shown; invented
//! dispositions are dropped harness-side regardless (docs/triage.md).
//!
//! Both descriptions carry the same standing clause — the tools are answered only when
//! the triage's tail instruction asks. Phrased as permission ("do not call unless asked"),
//! not identity: the same model reads this description and, later, the ask, so the
//! clause must resolve as "now is when" rather than contradict the ask.

use super::*;

/// The `inbox` triage schema field (seq-keyed pin/dismiss) — shared by `retain` and
/// the standalone `triage` tool.
pub(super) fn inbox_schema_field() -> Value {
    json!({
        "type": "object",
        "description": "If a section named \"inbox\" appears above, triage those pending notifications by [id]. Unlisted ids are left for the next decision point. Omit when no inbox section is shown.",
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
        "description": "If a section named \"background\" appears above, triage those live background processes by [p#n] handle. Unlisted handles keep streaming their tail at the next decision point. Omit when no background section is shown.",
        "properties": {
            "pin": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Handles whose current tail to capture onto the record now (e.g. \"p#3\")."
            },
            "mute": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Handles whose running tail to stop showing — they keep running and still notify on exit."
            }
        }
    })
}

/// The `surfaced` schema field (name-keyed). A plain string array: names are validated
/// against the roster in code (invented ones dropped), not by a schema enum, keeping
/// the guard harness-side (docs/triage.md).
pub(super) fn surfaced_schema_field() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "If a section named \"context\" appears above, the names of the items listed there that genuinely bear on what you're doing now — each is surfaced as a reminder to read it. Omit when no context section is shown or none apply."
    })
}

/// The fork triage's tool (docs/triage.md). Static — see the module doc.
///
/// Two answer shapes, because the schema is static while triage size is not. A triage
/// carrying one result — the overwhelming majority — is answered with the **flat**
/// top-level `keep`/`note`: naming the one call whose result is right there is
/// transcription work with no decision in it, and the id it asks for is the id most
/// likely to come back wrong. A triage carrying several is answered with the `results`
/// array, where a `result` label per entry (`r1`, `r2` — [`labels`](super::labels)) is
/// what makes the verdicts attributable.
/// Neither is `required`: the schema can't know the triage size, so the triage's tail
/// instruction ([`retain_instruction`](super::retain_instruction)) names the shape.
pub fn retain_tool() -> Value {
    json!({
        "name": RETAIN,
        "description": "Choose what of the tool output to keep on the record going forward. The output is line-numbered. Whatever you drop is gone from the working context, so keep anything that might matter — but stay lean. Do not call this tool unless asked to do so by the harness.",
        "input_schema": {
            "type": "object",
            "properties": {
                "keep": {
                    "type": "string",
                    "description": "The verdict when the harness asked about a single result: \"all\", \"none\", or comma-separated 1-based inclusive line ranges, e.g. \"40-52,310-318\" (a single line is \"7\"). No label needed — there is only one result in question."
                },
                "note": {
                    "type": "string",
                    "description": "One-line summary of what that output showed; the breadcrumb kept when little or nothing is retained."
                },
                "intent_feedback": {
                    "type": "string",
                    "description": "Optional; omit almost always. One clause, only when the call's intent gave you nothing to decide against, saying what it lacked so the next intent is one you can act on. About the intent, never about the task."
                },
                "results": {
                    "type": "array",
                    "description": "The verdict when the harness asked about several results at once — one entry per result it named, and only those. Line numbers restart at 1 within each result.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "result": {
                                "type": "string",
                                "description": "The label of the result this entry decides, from the header the harness showed it under, e.g. \"r1\"."
                            },
                            "keep": {
                                "type": "string",
                                "description": "\"all\", \"none\", or comma-separated 1-based inclusive line ranges, e.g. \"40-52,310-318\" (a single line is \"7\"). Numbered within this result only."
                            },
                            "note": {
                                "type": "string",
                                "description": "One-line summary of what this output showed; the breadcrumb kept when little or nothing is retained."
                            },
                            "intent_feedback": {
                                "type": "string",
                                "description": "Optional; omit almost always. One clause, only when this call's intent gave you nothing to decide against, saying what it lacked. About the intent, never about the task."
                            }
                        },
                        "required": ["result", "keep"]
                    }
                },
                "inbox": inbox_schema_field(),
                "background": background_schema_field(),
                "surfaced": surfaced_schema_field()
            }
        }
    })
}

/// The standalone idle triage's tool (docs/process-monitor.md, docs/inbox.md):
/// inbox/background dispositions only, no `keep` (there's no tool output). Static —
/// see the module doc.
pub fn triage_tool() -> Value {
    json!({
        "name": TRIAGE,
        "description": "Triage your pending notifications and running background processes. Pin what needs to enter your working context and be acted on now; dismiss noise; leave anything you're deferring unlisted (pinning nothing returns you to idle, unchanged). Do not call this tool unless asked to do so by the harness.",
        "input_schema": {
            "type": "object",
            "properties": {
                "inbox": inbox_schema_field(),
                "background": background_schema_field()
            }
        }
    })
}
