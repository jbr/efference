//! The `consolidate` call's arm of the cascade (docs/consolidate.md): an approved
//! `consolidate` acts on the log, not the world.

use super::*;

/// Record the summary and pair the `tool_use`. No world-run, no triage: there is no
/// tool output to curate, so the payloads land as they stand (`Decision::Direct`).
pub(super) fn record(shared: &Arc<Session>, call: &ToolCall) -> Result<Decision, Error> {
    let note = consolidate::summary(&call.input).unwrap_or_default();
    let log = shared.snapshot();
    let current = log
        .iter()
        .position(|e| match &e.payload {
            Payload::Assistant { output, .. } => output
                .tool_calls()
                .is_ok_and(|calls| calls.iter().any(|c| c.id == call.id)),
            _ => false,
        })
        .unwrap_or(log.len());
    let prefix = &log[..current];
    let span = prefix
        .iter()
        .rev()
        .find(|e| {
            matches!(
                e.payload,
                Payload::User(_) | Payload::Assistant { .. } | Payload::ToolResult { .. }
            )
        })
        .and_then(|e| match &e.payload {
            Payload::User(_) => consolidate::candidate(prefix),
            Payload::Assistant { output, .. }
                if output.tool_calls().is_ok_and(|calls| calls.is_empty()) =>
            {
                consolidate::candidate(prefix)
            }
            _ => consolidate::before(prefix, e.seq),
        });
    let refusal = match span {
        None => Some(
            "consolidate refused: there is no completed work since the previous \
                     consolidation to replace; nothing was consolidated",
        ),
        Some(_) if note.split_whitespace().count() < 100 => Some(
            "consolidate refused: the summary is shorter than 100 words; nothing \
                     was consolidated",
        ),
        Some(_) => None,
    };
    let (Some(span), None) = (span, refusal) else {
        return Ok(Decision::Direct(vec![Payload::tool_result(
            call.id.clone(),
            RawResult::error(refusal.unwrap_or_default()),
            WireState::Raw,
        )]));
    };
    Ok(Decision::Direct(vec![
        Payload::Summary(Summary::consolidation(span.from, span.upto, note)),
        Payload::tool_result(
            call.id.clone(),
            RawResult::text("consolidated"),
            WireState::Raw,
        ),
    ]))
}
