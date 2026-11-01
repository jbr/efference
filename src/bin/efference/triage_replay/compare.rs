//! Comparing a replayed triage's verdicts against the recorded ones, and printing the
//! table (docs/triage.md).

use super::Recorded;
use efference::record::log::{Keep, WireState};
use efference::record::project::selection;
use efference::triage::ReplayVerdict;
use std::collections::BTreeSet;

/// Collapse `s` onto one line and cap it at `max` characters — a failing draw's arguments
/// and prose are for scanning a sweep, not for reading in full.
pub(super) fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// The size of the peek's trailing user message — the part of the request that is fresh
/// on every triage, as against the prefix it re-reads warm.
pub(super) fn tail_chars(turns: &[efference::record::project::WireTurn]) -> usize {
    use efference::record::project::{UserPart, WireTurn};
    let Some(WireTurn::User(parts)) = turns.last() else {
        return 0;
    };
    parts
        .iter()
        .map(|p| match p {
            UserPart::Text(text) => text.len(),
            UserPart::ToolResult { content, .. } => content.len(),
            UserPart::Breakpoint => 0,
        })
        .sum()
}

/// The one-line description of what a replayed attempt did.
pub(super) fn outcome(attempt: &efference::triage::TriageAttempt) -> String {
    match &attempt.why {
        None => format!(
            "ok · keep {}",
            attempt.raw_keep.as_deref().unwrap_or("(none)")
        ),
        Some(why) => why.clone(),
    }
}

/// The 1-based line numbers a verdict keeps over `content`, after the cap.
pub(super) fn kept_set(content: &str, keep: &Keep) -> BTreeSet<usize> {
    selection(content, keep)
        .ranges
        .iter()
        .flat_map(|&(a, b)| a..=b)
        .collect()
}

/// One result's comparison: a candidate verdict against the recorded one.
#[derive(serde::Serialize)]
pub(super) struct Compared {
    pub(super) seq: u64,
    pub(super) draw: usize,
    pub(super) call_id: String,
    pub(super) tool: String,
    pub(super) intent: Option<String>,
    pub(super) shown: usize,
    pub(super) reference_keep: String,
    pub(super) candidate_keep: String,
    pub(super) reference_kept: usize,
    pub(super) candidate_kept: usize,
    /// Lines the candidate kept that the reference dropped.
    pub(super) over: usize,
    /// Lines the reference kept that the candidate dropped.
    pub(super) under: usize,
    pub(super) reference_note: Option<String>,
    pub(super) candidate_note: Option<String>,
    /// Wall-clock seconds the draw took, request to verdict. The triage sits on the
    /// critical path of every tool call, so this is one of the three axes beside
    /// judgment and price.
    pub(super) seconds: f64,
    /// `retain(none)` against a reference that kept something — the shape of the
    /// "no content was shown" verdicts seen on a real read.
    pub(super) phantom_none: bool,
}

/// The `Keep` a recorded wire state carries, if the reference is a model verdict.
/// A bypassed (`Raw`) or withheld result has no reference to compare against.
pub(super) fn recorded_keep(wire: &WireState) -> Option<(&Keep, Option<&str>)> {
    match wire {
        WireState::Triaged(v) => Some((&v.keep, v.note.as_deref())),
        _ => None,
    }
}

pub(super) fn keep_label(keep: &Keep) -> String {
    match keep {
        Keep::All => "all".into(),
        Keep::None => "none".into(),
        Keep::Ranges(rs) => rs
            .iter()
            .map(|(a, b)| {
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}-{b}")
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

/// Compare one draw's verdicts against the triage's recorded ones, result by result.
pub(super) fn compare(
    triage: &Recorded,
    draw: usize,
    verdicts: &[ReplayVerdict],
    seconds: f64,
) -> Vec<Compared> {
    let mut out = Vec::new();
    for ((call, raw), wire) in triage.calls.iter().zip(&triage.raws).zip(&triage.recorded) {
        let Some((reference, reference_note)) = recorded_keep(wire) else {
            continue;
        };
        let Some(candidate) = verdicts.iter().find(|v| v.call_id == call.id.0) else {
            continue;
        };
        let shown = selection(&raw.content, &Keep::All).shown.len();
        let r = kept_set(&raw.content, reference);
        let c = kept_set(&raw.content, &candidate.keep);
        out.push(Compared {
            seq: triage.seq,
            draw,
            call_id: call.id.0.clone(),
            tool: call.name.clone(),
            intent: call.intent().map(str::to_string),
            shown,
            reference_keep: keep_label(reference),
            candidate_keep: keep_label(&candidate.keep),
            reference_kept: r.len(),
            candidate_kept: c.len(),
            over: c.difference(&r).count(),
            under: r.difference(&c).count(),
            reference_note: reference_note.map(str::to_string),
            candidate_note: candidate.note.clone(),
            seconds,
            phantom_none: matches!(candidate.keep, Keep::None) && !r.is_empty(),
        });
    }
    out
}

pub(super) fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "–".into()
    } else {
        format!("{:.0}%", 100.0 * n as f64 / d as f64)
    }
}

/// Print the comparison table: every compared result sorted by disagreement, the
/// worst first, capped at `rows`. Where the reading happens.
pub(super) fn print_table(rows_cap: usize, compared: &[Compared]) {
    let mut rows: Vec<&Compared> = compared.iter().collect();
    rows.sort_by_key(|c| std::cmp::Reverse(c.over + c.under));
    println!(
        "\n| seq | draw | tool | shown | reference | candidate | over | under | reference note | candidate note |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for c in rows.iter().take(rows_cap) {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.seq,
            c.draw,
            c.tool,
            c.shown,
            one_line(&c.reference_keep, 24),
            one_line(&c.candidate_keep, 24),
            pct(c.over, c.shown),
            pct(c.under, c.shown),
            one_line(c.reference_note.as_deref().unwrap_or(""), 120),
            one_line(c.candidate_note.as_deref().unwrap_or(""), 120),
        );
    }
    if compared.len() > rows_cap {
        println!(
            "\n({} more rows; `--table` raises the cap, `--out` writes every row)",
            compared.len() - rows_cap
        );
    }
}
