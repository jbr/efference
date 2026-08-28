//! Deriving one call's [`ToolTelemetry`] at the dispatch seam (docs/tools.md).
//!
//! Everything here reads what the seam already holds — the call's own `input`,
//! the [`RawResult`] the backend returned, and a clock started immediately
//! before dispatch. Nothing is plumbed out of the tool crates for it, which is
//! what keeps the record cheap enough to write on **every** call rather than on
//! the ones someone thought to instrument.

use super::*;

use crate::log::{BashTelemetry, EditTelemetry, ToolOutcome, ToolTelemetry};

/// Argument names an edit-shaped call spells its anchor with. Several tools
/// across two crates use one of these; a call carrying none simply records no
/// [`EditTelemetry`], which is the same answer as "this was not an edit".
const ANCHOR_KEYS: [&str; 4] = ["anchor", "selector", "old_text", "old_string"];

/// Argument names for the replacement half, in the same spirit.
const CONTENT_KEYS: [&str; 4] = ["content", "new_content", "new_text", "new_string"];

/// Measure one completed call.
///
/// `harness_error` is the caller's answer to the one question the result cannot
/// answer for itself: `is_error` says the call did not succeed, not *whose*
/// failure it was, and the difference is the whole point of the third outcome
/// class (a spawn that failed is a defect signal; an anchor that matched nothing
/// is a datum about the model). It is set at the two sites that construct a
/// harness-authored failure — never inferred from the text, which would put the
/// classification back where it started.
pub(super) fn observe(
    call: &ToolCall,
    result: &RawResult,
    elapsed: std::time::Duration,
    harness_error: bool,
) -> ToolTelemetry {
    let outcome = match (harness_error, result.is_error) {
        (true, _) => ToolOutcome::HarnessError,
        (false, true) => ToolOutcome::Rejected,
        (false, false) => ToolOutcome::Applied,
    };
    ToolTelemetry {
        call_id: call.id.clone(),
        tool: call.name.clone(),
        // Saturating rather than wrapping: a duration past u64 milliseconds is
        // not reachable, and a wrapped one would read as a fast call.
        duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        outcome,
        result_bytes: result.content.len(),
        images: result.images.len(),
        bash: (call.name == bash::NAME).then(|| BashTelemetry {
            exit_status: crate::bash::reported_exit_status(&result.content).map(str::to_string),
            background: bash::is_background_call(call),
        }),
        edit: edit_telemetry(call),
    }
}

/// The edit-shaped fields, when the call's arguments carry them. `None` for a
/// call that names neither an anchor nor replacement content — which is most
/// calls, and keeps the recorded entry small.
fn edit_telemetry(call: &ToolCall) -> Option<EditTelemetry> {
    let operation = call
        .input
        .get("operation")
        .and_then(Value::as_str)
        .map(str::to_string);
    let anchor_bytes = first_str_len(call, &ANCHOR_KEYS);
    let content_bytes = first_str_len(call, &CONTENT_KEYS);
    (operation.is_some() || anchor_bytes.is_some() || content_bytes.is_some()).then_some(
        EditTelemetry {
            operation,
            anchor_bytes,
            content_bytes,
        },
    )
}

/// The byte length of the first of `keys` the call supplied as a string.
fn first_str_len(call: &ToolCall, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|key| call.input.get(*key).and_then(Value::as_str))
        .map(str::len)
}

#[cfg(test)]
mod test;
