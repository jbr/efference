//! Rendering a triaged verdict back to wire text as `@@` hunks (docs/triage.md).

use super::*;

/// The line an interrupted call projects in place of its output (docs/control-channel.md):
/// who interrupted and what became of the call. Rendered from the recorded
/// [`Interruption`](crate::log::Interruption) alone, so it is byte-stable.
pub fn render_interruption(interruption: &crate::log::Interruption) -> String {
    let who = match interruption.by.as_deref() {
        None | Some("web") => "the operator".into(),
        Some(source) => crate::frontend::wire_label(source),
    };
    match interruption.after_ms {
        Some(ms) => {
            let what = match interruption.killed {
                true => "the command was killed",
                false => "the call finished",
            };
            let secs = ms / 1000;
            let after = match secs {
                0..60 => format!("{secs}s"),
                _ => format!("{}m {}s", secs / 60, secs % 60),
            };
            format!("[interrupted by {who} after {after}: {what}, and its output was withheld]")
        }
        None => format!("[interrupted by {who} before its result was read]"),
    }
}

/// Render a [`WireState::TriageFailed`] result (docs/triage.md): a hard error
/// in place of the output, phrased for the agent exactly as a failed call would be —
/// naming that the call *did* run, so a mutation isn't blindly re-issued. The stable
/// "result withheld — triage failed" prefix is the greppable marker. The wire
/// projection and the TUI projection share this one renderer; like
/// [`render_triaged`] it is deterministic, a pure function of its input.
///
/// Three wording constraints, each one an agent misreading the text can produce.
/// It must not say the output was *not recorded* — the raw sits in `log.jsonl`,
/// and only the agent-facing projection lost it. It must attribute the failure to
/// the harness, or an agent reading a triage's rate limit blames its own tool and
/// re-issues the call in shrinking forms. And the re-issue advice stays
/// conditional, because re-running is not always safe: a withheld verdict on a
/// call whose side effects already stand can turn a real pass into a spurious
/// failure when the call is repeated.
///
/// [`WireState::TriageFailed`]: crate::log::WireState::TriageFailed
pub fn render_triage_failure(why: &str) -> String {
    format!(
        "result withheld — triage failed ({why}). This is a harness failure, not a \
         failure of your call: the call ran to completion and its side effects stand, \
         and its raw output is in the session log — only the step that curates it for \
         you did not complete. Re-issue the call if its output is still needed and \
         running it again is safe."
    )
}

/// The first line of a result triage kept nothing of (docs/triage.md). A bare `⟢`
/// note would read as the harness's ask for `retain`.
pub const NOTHING_KEPT: &str = "(output dropped by triage)";

/// The fixed lead on a rendered `intent_feedback` line (docs/triage.md).
pub const INTENT_FEEDBACK_LEAD: &str = "⟢ re your intent: ";

/// Render a triaged tool result for the canonical wire from `raw` + the stored
/// verdict (docs/triage.md): the kept ranges as diff-style `@@`-hunks over the verbatim
/// lines, the optional `note`, and a truncation breadcrumb past the cap. The wire
/// projection and the TUI projection share this one renderer. **Deterministic** — part
/// of the byte-stability contract (docs/log-and-projection.md): a pure function of its inputs.
pub fn render_triaged(content: &str, verdict: &Triaged) -> String {
    let Selection {
        shown,
        ranges,
        dropped,
    } = selection(content, &verdict.keep);
    let mut out = render_hunks(&shown, &ranges, verdict, shown.len() + dropped);
    if dropped > 0 {
        out.push('\n');
        out.push_str(&format!("… [{dropped} more lines truncated before triage]"));
    }
    out
}

/// Which lines a verdict actually keeps, as structure rather than as text.
///
/// The wire has to say "these lines came from there" in words, so it formats this
/// as `@@` hunks; the TUI has a gutter and renders the same lines numbered
/// (docs/tui.md). Both go through here, because the one thing that must not drift
/// is *which lines* each shows — an operator comparing their view against the
/// agent's is the whole point of the view (docs/positions.md).
pub struct Selection<'c> {
    /// The triage the model was shown: the content capped at [`TRIAGE_LINE_CAP`] lines.
    pub shown: Vec<&'c str>,
    /// Kept ranges over `shown`, 1-based and inclusive, normalized (clamped,
    /// sorted, overlaps merged).
    pub ranges: Vec<(usize, usize)>,
    /// Lines the cap dropped before triage ever saw them.
    pub dropped: usize,
}

impl Selection<'_> {
    /// The kept lines paired with their original 1-based numbers, ascending. Gaps
    /// in the numbering are the gaps between ranges.
    pub fn numbered_lines(&self) -> Vec<(u32, &str)> {
        self.ranges
            .iter()
            .flat_map(|&(start, end)| (start..=end).map(|i| (i as u32, self.shown[i - 1])))
            .collect()
    }
}

/// Resolve a verdict against its raw content — the cap, then the kept ranges.
pub fn selection<'c>(content: &'c str, keep: &Keep) -> Selection<'c> {
    let all: Vec<&'c str> = content.lines().collect();
    let total = all.len();
    let shown = all[..total.min(TRIAGE_LINE_CAP)].to_vec();
    let dropped = total - shown.len();
    let ranges: Vec<(usize, usize)> = match keep {
        Keep::All => vec![(1, shown.len())],
        Keep::None => vec![],
        Keep::Ranges(rs) => rs.clone(),
    };
    let ranges = normalize(&ranges, shown.len());
    Selection {
        shown,
        ranges,
        dropped,
    }
}

/// Render kept ranges as diff-style hunks: a `@@ lines a-b @@` header per contiguous
/// span (`@@ line n @@` for one line), then those lines *verbatim* (unnumbered). The
/// header carries the position anchor at one line per *range*; gaps between hunks are
/// implied by the jump in the headers, as in a unified diff, and a closing
/// `@@ n lines total @@` (`total` counts lines the cap dropped before triage too) follows when the last range stops before the last line with
/// content. A verdict that keeps nothing opens with [`NOTHING_KEPT`] before any note. The verdict's optional
/// trailing note is the breadcrumb kept even when nothing is retained
/// (docs/triage.md) — one overall note, so it survives the empty-hunks case.
/// Its `intent_feedback`, when present, follows the note under the same `⟢` sign with a
/// fixed `re your intent:` lead, so the agent reads it as the curator's remark about the
/// channel rather than about the output. `ranges` is passed resolved rather than read
/// off `verdict.keep`, since the caller has already clamped it against the cap.
pub(super) fn render_hunks(
    lines: &[&str],
    ranges: &[(usize, usize)],
    verdict: &Triaged,
    total: usize,
) -> String {
    let ranges = normalize(ranges, lines.len());
    // The last line with content. Blank lines after it carry nothing to cut, so a
    // range reaching it covers the result (the shape `read`'s frame produces ends in
    // an empty line).
    let content_end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    // A header marks *where* curation cut. One range spanning everything cut nothing,
    // so its header states only "all of it" — noise on every fully-retained result,
    // and on a one-line result it is longer than the line it labels. Truncation is
    // reported separately by the caller, so suppressing this hides nothing
    // (docs/triage.md).
    let spans_everything = matches!(ranges.as_slice(), [(1, end)] if *end >= content_end);
    let mut parts: Vec<String> = Vec::new();
    if ranges.is_empty() && !lines.is_empty() {
        parts.push(NOTHING_KEPT.to_string());
    }
    for &(start, end) in &ranges {
        if !spans_everything {
            parts.push(hunk_header(start, end));
        }
        for i in start..=end {
            parts.push(lines[i - 1].to_string());
        }
    }
    // Nothing marks the cut after the last range otherwise: the headers only show
    // jumps between ranges.
    if !spans_everything && ranges.last().is_some_and(|&(_, end)| end < content_end) {
        parts.push(format!("@@ {total} lines total @@"));
    }
    if let Some(note) = verdict
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("⟢ {note}"));
    }
    if let Some(fb) = verdict
        .intent_feedback
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("{INTENT_FEEDBACK_LEAD}{fb}"));
    }
    if parts.is_empty() {
        parts.push("(nothing kept)".to_string());
    }
    parts.join("\n")
}

/// A unified-diff-style hunk header: `@@ lines a-b @@`, or `@@ line n @@` for a single
/// kept line.
pub(super) fn hunk_header(start: usize, end: usize) -> String {
    if start == end {
        format!("@@ line {start} @@")
    } else {
        format!("@@ lines {start}-{end} @@")
    }
}

/// Clamp ranges to `[1, n]`, drop out-of-range, sort, and merge overlapping/adjacent.
/// Tolerates reversed ranges.
pub(super) fn normalize(ranges: &[(usize, usize)], n: usize) -> Vec<(usize, usize)> {
    if n == 0 {
        return Vec::new();
    }
    let mut rs: Vec<(usize, usize)> = ranges
        .iter()
        .map(|&(s, e)| (s.min(e), s.max(e)))
        .filter(|&(s, _)| s >= 1 && s <= n) // drop wholly-out-of-range
        .map(|(s, e)| (s, e.min(n))) // clamp the tail down to n
        .collect();
    rs.sort_by_key(|&(s, _)| s);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in rs {
        match merged.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// The restart notice a [`Resumed`](crate::log::Resumed) entry projects
/// (docs/log-and-projection.md): a harness line saying the session was reopened,
/// how long it sat, and what the loader set aside. `at` is the entry's own seq, which
/// bounds the skipped range. It states what happened and
/// nothing about what to do next.
pub fn render_resumed(resumed: &crate::log::Resumed, at: crate::log::Seq) -> String {
    let secs = resumed.gap_ms / 1000;
    let gap = match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86400, secs % 86400 / 3600),
    };
    let mut out = format!("⟢ session resumed after {gap}: the host process restarted.");
    if !resumed.lost_processes.is_empty() {
        out.push_str(&format!(
            " Background processes no longer monitored, output no longer captured, \
             still running or not unknown: {}.",
            resumed.lost_processes.join(", ")
        ));
    }
    if !resumed.pending_calls.is_empty() {
        let count = resumed.pending_calls.len();
        let (noun, pronoun) = if count == 1 {
            ("call", "it")
        } else {
            ("calls", "them")
        };
        out.push_str(&format!(
            " The session stopped with {count} tool {noun} pending; the operator's next \
             action brings {pronoun} back."
        ));
    }
    let change = &resumed.tools_changed;
    if !change.is_empty() {
        let mut parts = Vec::new();
        for (label, names) in [
            ("added", &change.added),
            ("removed", &change.removed),
            ("changed", &change.changed),
        ] {
            if !names.is_empty() {
                let names: Vec<String> = names
                    .iter()
                    .map(|name| crate::mcp::display_name(name))
                    .collect();
                parts.push(format!("{label}: {}", names.join(", ")));
            }
        }
        if change.reordered {
            parts.push("same tools in a different order".to_string());
        }
        out.push_str(&format!(
            " The tools available differ from before the restart ({}).",
            parts.join("; ")
        ));
    }
    let skipped = resumed.skipped(at);
    if !skipped.is_empty() {
        let count = skipped.end - skipped.start;
        let (noun, were, they) = if count == 1 {
            ("entry", "was", "it may include a tool call that ran")
        } else {
            ("entries", "were", "they may include tool calls that ran")
        };
        out.push_str(&format!(
            " The session did not end cleanly: {count} {noun} after seq {} {were} not \
             restored, and {they}.",
            skipped.start - 1
        ));
    }
    out
}
