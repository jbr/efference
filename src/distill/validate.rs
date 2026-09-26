//! Verdict validation against gate offerings and bounds (docs/redaction-fork.md).

use super::*;

/// Verdict validity includes **coverage** (docs/redaction-fork.md): every offered
/// label must be answered, or the verdict is rejected and retried in-fork. This gate has no
/// keep-all fallback, so a result silently left untriaged has nowhere to land.
///
/// The rejection names what the verdict *did* answer, not only what it missed. Without
/// that, "answered nothing" and "answered twenty-six results that were never offered"
/// are the same line in the log, and the second is a prompt-scoping defect the first
/// gives no hint of.
pub(super) fn require_coverage(
    verdict: ParsedRetain,
    offered: &[String],
) -> Result<ParsedRetain, String> {
    let unanswered: Vec<&str> = offered
        .iter()
        .filter(|id| !verdict.results.iter().any(|v| &v.label == *id))
        .map(String::as_str)
        .collect();
    if unanswered.is_empty() {
        return Ok(verdict);
    }
    let answered = match verdict.results.as_slice() {
        [] => "nothing".to_string(),
        answered => format!(
            "[{}]",
            answered
                .iter()
                .map(|v| v.label.as_str())
                .collect::<Vec<_>>()
                .join("], [")
        ),
    };
    Err(format!(
        "verdict answered no result for [{}] — it answered {answered}",
        unanswered.join("], [")
    ))
}

/// The number of numbered lines the peek view shows for a raw result — the bound a
/// `keep` range must land inside. Counts the way [`result_view`] numbers and
/// `distillate` caps (docs/redaction-fork.md): all three must agree or the verdict's
/// coordinates drift from what the model saw.
pub(super) fn shown_line_count(raw: &RawResult) -> usize {
    raw.content.lines().count().min(PEEK_LINE_CAP)
}

/// Each offered label's shown line count, the bound table [`require_in_bounds`] checks
/// against. `labels` is [`labels`](super::labels) for the same `pending`.
pub(super) fn shown_lines(
    pending: &[Pending<'_>],
    labels: &[Option<String>],
) -> Vec<(String, usize)> {
    pending
        .iter()
        .zip(labels)
        .filter_map(|(p, label)| Some((label.clone()?, shown_line_count(p.raw))))
        .collect()
}

/// Verdict validity also includes **bounds** (docs/redaction-fork.md): a `keep` range
/// lying wholly outside the shown line numbers is a coordinate mistake, not a decision
/// (a peek adopting the file line numbers embedded in `grep -n` output as its retain
/// coordinates) — and render-time normalization would drop it, turning
/// "keep these lines" into silent total loss with no signal on either side. Rejected
/// here instead, the mistake becomes an in-fork retry whose nudge names the actual
/// bound. One out-of-range range fails the whole verdict: a partial drop is the same
/// silent loss at smaller scale, and the retry is a warm prefix extension.
///
/// Overshoot from an in-bounds start stays acceptable (render clamps the tail — the
/// peek's line accounting routinely runs a line or two past the rendering's). A 0 start
/// is rejected rather than clamped: `0-5` under zero-indexing means shown lines 1–6,
/// as a typo'd lower bound it means 1–5, and guessing either silently keeps the wrong
/// lines.
///
/// `shown` maps each offered label to its shown line count; a verdict keyed to a label
/// that was never offered has no bound to check and passes through — the caller's
/// reconciliation drops it, the same guard as everywhere else.
pub(super) fn require_in_bounds(
    verdict: ParsedRetain,
    shown: &[(String, usize)],
) -> Result<ParsedRetain, String> {
    for v in &verdict.results {
        let Keep::Ranges(ranges) = &v.keep else {
            continue;
        };
        let Some(&(_, n)) = shown.iter().find(|(id, _)| *id == v.label) else {
            continue;
        };
        for &(s, e) in ranges {
            let range = if s == e {
                format!("line {s}")
            } else {
                format!("range {s}-{e}")
            };
            if s.min(e) == 0 {
                return Err(format!(
                    "keep {range} for [{}] starts at 0 — the shown line numbers start at 1",
                    v.label
                ));
            }
            if s.min(e) > n {
                return Err(format!(
                    "keep {range} for [{}] is beyond the shown output, which has {n} \
                     lines — keep ranges index the line numbers shown, not numbers \
                     inside the output",
                    v.label
                ));
            }
        }
    }
    Ok(verdict)
}

/// Describe a completion that produced no usable verdict, for the `Triage` breadcrumb.
/// The model's actual input is the most useful form when the asked-for tool was called —
/// that is what a future reader needs to tell a malformed verdict from a wrong-tool call.
pub(super) fn unusable(completion: &Completion, tool: &str) -> String {
    let Payload::Assistant { output, .. } = &completion.assistant else {
        return "peek returned no assistant payload".into();
    };
    let Ok(calls) = output.tool_calls() else {
        return "peek returned an unparseable tool call".into();
    };
    match calls.iter().find(|c| c.name == tool) {
        Some(c) => format!("unparseable {tool} input: {}", c.input),
        None if calls.is_empty() => "peek called no tool".into(),
        None => format!(
            "peek called {} instead of {tool}",
            calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
