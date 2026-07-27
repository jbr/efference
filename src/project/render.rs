//! Rendering a distilled verdict back to wire text as `@@` hunks (docs/redaction-fork.md).

use super::*;

/// Render a distilled tool result for the canonical wire from `raw` + the stored
/// verdict (docs/redaction-fork.md): the kept ranges as diff-style `@@`-hunks over the verbatim
/// lines, the optional `note`, and a truncation breadcrumb past the cap. The wire
/// projection and the TUI projection share this one renderer. **Deterministic** — part
/// of the byte-stability contract (docs/log-and-projection.md): a pure function of its inputs.
pub fn render_distilled(content: &str, keep: &Keep, note: Option<&str>) -> String {
    let all: Vec<&str> = content.lines().collect();
    let total = all.len();
    let shown = &all[..total.min(PEEK_LINE_CAP)];
    let dropped = total - shown.len();
    let ranges: Vec<(usize, usize)> = match keep {
        Keep::All => vec![(1, shown.len())],
        Keep::None => vec![],
        Keep::Ranges(rs) => rs.clone(),
    };
    let mut out = render_hunks(shown, &ranges, note);
    if dropped > 0 {
        out.push('\n');
        out.push_str(&format!("… [{dropped} more lines truncated before triage]"));
    }
    out
}

/// Render kept ranges as diff-style hunks: a `@@ lines a-b @@` header per contiguous
/// span (`@@ line n @@` for one line), then those lines *verbatim* (unnumbered). The
/// header carries the position anchor at one line per *range*; gaps between hunks are
/// implied by the jump in the headers, as in a unified diff. An optional trailing note
/// is the breadcrumb kept even when nothing is retained (docs/redaction-fork.md) — one
/// overall note, so it survives the empty-hunks case.
pub(super) fn render_hunks(
    lines: &[&str],
    ranges: &[(usize, usize)],
    note: Option<&str>,
) -> String {
    let ranges = normalize(ranges, lines.len());
    let mut parts: Vec<String> = Vec::new();
    for &(start, end) in &ranges {
        parts.push(hunk_header(start, end));
        for i in start..=end {
            parts.push(lines[i - 1].to_string());
        }
    }
    if let Some(note) = note.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("⟢ {note}"));
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
