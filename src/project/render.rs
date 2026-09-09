//! Rendering a distilled verdict back to wire text as `@@` hunks (docs/redaction-fork.md).

use super::*;

/// Render a [`WireState::TriageFailed`] result (docs/redaction-fork.md): a hard error
/// in place of the output, phrased for the agent exactly as a failed call would be —
/// naming that the call *did* run, so a mutation isn't blindly re-issued. The stable
/// "result withheld — triage failed" prefix is the greppable marker. The wire
/// projection and the TUI projection share this one renderer; like
/// [`render_distilled`] it is deterministic, a pure function of its input.
///
/// Three wording constraints, each one an agent misreading the text can produce.
/// It must not say the output was *not recorded* — the raw sits in `log.jsonl`,
/// and only the agent-facing projection lost it. It must attribute the failure to
/// the harness, or an agent reading a peek's rate limit blames its own tool and
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

/// The fixed lead on a rendered `intent_feedback` line (docs/redaction-fork.md).
pub const INTENT_FEEDBACK_LEAD: &str = "⟢ re your intent: ";

/// Render a distilled tool result for the canonical wire from `raw` + the stored
/// verdict (docs/redaction-fork.md): the kept ranges as diff-style `@@`-hunks over the verbatim
/// lines, the optional `note`, and a truncation breadcrumb past the cap. The wire
/// projection and the TUI projection share this one renderer. **Deterministic** — part
/// of the byte-stability contract (docs/log-and-projection.md): a pure function of its inputs.
pub fn render_distilled(content: &str, verdict: &Distilled) -> String {
    let Distillate {
        shown,
        ranges,
        dropped,
    } = distillate(content, &verdict.keep);
    let mut out = render_hunks(&shown, &ranges, verdict);
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
pub struct Distillate<'c> {
    /// The peek the model was shown: the content capped at [`PEEK_LINE_CAP`] lines.
    pub shown: Vec<&'c str>,
    /// Kept ranges over `shown`, 1-based and inclusive, normalized (clamped,
    /// sorted, overlaps merged).
    pub ranges: Vec<(usize, usize)>,
    /// Lines the cap dropped before triage ever saw them.
    pub dropped: usize,
}

impl Distillate<'_> {
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
pub fn distillate<'c>(content: &'c str, keep: &Keep) -> Distillate<'c> {
    let all: Vec<&'c str> = content.lines().collect();
    let total = all.len();
    let shown = all[..total.min(PEEK_LINE_CAP)].to_vec();
    let dropped = total - shown.len();
    let ranges: Vec<(usize, usize)> = match keep {
        Keep::All => vec![(1, shown.len())],
        Keep::None => vec![],
        Keep::Ranges(rs) => rs.clone(),
    };
    let ranges = normalize(&ranges, shown.len());
    Distillate {
        shown,
        ranges,
        dropped,
    }
}

/// Render kept ranges as diff-style hunks: a `@@ lines a-b @@` header per contiguous
/// span (`@@ line n @@` for one line), then those lines *verbatim* (unnumbered). The
/// header carries the position anchor at one line per *range*; gaps between hunks are
/// implied by the jump in the headers, as in a unified diff. The verdict's optional
/// trailing note is the breadcrumb kept even when nothing is retained
/// (docs/redaction-fork.md) — one overall note, so it survives the empty-hunks case.
/// Its `intent_feedback`, when present, follows the note under the same `⟢` sign with a
/// fixed `re your intent:` lead, so the agent reads it as the curator's remark about the
/// channel rather than about the output. `ranges` is passed resolved rather than read
/// off `verdict.keep`, since the caller has already clamped it against the cap.
pub(super) fn render_hunks(
    lines: &[&str],
    ranges: &[(usize, usize)],
    verdict: &Distilled,
) -> String {
    let ranges = normalize(ranges, lines.len());
    // A header marks *where* curation cut. One range spanning everything cut nothing,
    // so its header states only "all of it" — noise on every fully-retained result,
    // and on a one-line result it is longer than the line it labels. Truncation is
    // reported separately by the caller, so suppressing this hides nothing
    // (docs/redaction-fork.md).
    let spans_everything = matches!(ranges.as_slice(), [(1, end)] if *end == lines.len());
    let mut parts: Vec<String> = Vec::new();
    for &(start, end) in &ranges {
        if !spans_everything {
            parts.push(hunk_header(start, end));
        }
        for i in start..=end {
            parts.push(lines[i - 1].to_string());
        }
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
