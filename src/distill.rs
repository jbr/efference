//! Distillation — what a raw tool result projects to on the canonical wire
//! (DESIGN §4-realized, §10 Phase 1/2).
//!
//! Every tool result is distilled at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The distiller is the pluggable policy (§5) behind a
//! trait:
//!
//! - [`HeadTail`] — a deterministic stub (first `head` + last `tail` lines).
//! - [`ForkPeek`] — the §4 universal triage gate: fork a throwaway branch off the
//!   cached prefix, show the model the full line-numbered output, and let it call
//!   `retain` to choose what the canonical wire keeps. One tool call per turn
//!   (§8), so the fork is always single-result. On any failure it keeps
//!   everything — never loses data.
//!
//! Two distinct renderings, deliberately split:
//!
//! - [`number_lines`] — every line `cat -n`-numbered. The coordinate system the
//!   peek addresses with `retain(lines=…)`, shown ONLY in the throwaway fork.
//! - [`render_hunks`] — what lands on the **canonical wire**: kept ranges as
//!   diff-style `@@ lines a-b @@` hunks, the lines verbatim (unnumbered). The
//!   header carries the position anchor at one line per *range* instead of a
//!   number on every *line* — per-line numbering was overhead heavy enough to
//!   make distillates fatter than the raw they replaced (measured across a live
//!   session); gaps between hunks are implied by the jump in the headers.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Completion, ToolChoice};
use crate::log::{LogEntry, Payload, RawResult, ToolCall, WireState};
use crate::project::{ApiBlock, ApiMessage, Role, place_trailing_cache_breakpoint, project};

const RETAIN: &str = "retain";

/// Hard cap on how many lines the §4 peek is shown, applied *before* the model
/// call — a safety floor against a pathologically large tool output reaching the
/// API. The full raw is always kept in the log; only the peek (and thus the
/// distillate) is bounded. Generous on purpose — normal outputs are untouched.
const PEEK_LINE_CAP: usize = 2000;

const RETAIN_INSTRUCTION: &str = "Above is the FULL, line-numbered output of the \
tool call you just made. Call `retain` to choose what stays on the record going \
forward: \"all\", \"none\", or comma-separated 1-based inclusive line ranges like \
\"40-52,310-318\". Whatever you drop is gone from the working context — so keep \
anything that might matter for the rest of the task, while staying as lean as you \
can. Always add a one-line `note` summarizing what the output showed.";

/// Decide the wire form of a tool result (DESIGN §4). `prefix` is the log through
/// the assistant turn that made `call` (the fork's cached head); `client` lets a
/// model-driven distiller run the fork. `raw` is never mutated — it stays in the
/// log.
#[allow(async_fn_in_trait)] // single-threaded loop; no Send bound needed.
pub trait Distiller {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        call: &ToolCall,
        raw: &RawResult,
    ) -> WireState;
}

/// Deterministic stub: keep the first `head` + last `tail` lines. Ignores the
/// fork context — for tests and as the model-free fallback.
pub struct HeadTail {
    pub head: usize,
    pub tail: usize,
}

impl Distiller for HeadTail {
    async fn distill(
        &self,
        _client: &Client,
        _prefix: &[LogEntry],
        _call: &ToolCall,
        raw: &RawResult,
    ) -> WireState {
        let lines: Vec<&str> = raw.content.lines().collect();
        let n = lines.len();
        let ranges = if n <= self.head + self.tail {
            vec![(1, n)]
        } else {
            vec![(1, self.head), (n - self.tail + 1, n)]
        };
        WireState::Distilled(render_hunks(&lines, &ranges, None))
    }
}

/// The model peek — the §4 universal triage gate realized.
pub struct ForkPeek;

impl Distiller for ForkPeek {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        call: &ToolCall,
        raw: &RawResult,
    ) -> WireState {
        // Cap what the peek sees before any model call (the full raw still goes
        // to the log; only the peek + distillate work over the first N lines).
        let all: Vec<&str> = raw.content.lines().collect();
        let lines = &all[..all.len().min(PEEK_LINE_CAP)];
        let dropped = all.len() - lines.len();
        // The peek sees the full (capped) output with a number on every line —
        // the coordinate system `retain(lines=…)` addresses. This per-line
        // numbering lives ONLY in the throwaway fork; the canonical wire uses the
        // far leaner diff-style hunks (see `render_hunks`).
        let mut peek_view = number_lines(lines);
        if dropped > 0 {
            peek_view.push('\n');
            peek_view.push_str(&format!("       … [{dropped} more lines truncated] …"));
        }

        // Fork: cached prefix-through-tool_use ┃ + full result + retain prompt.
        // The prefix hits cache; only the raw + instruction tail is fresh, and it
        // is discarded — the canonical lineage never sees raw (DESIGN §4).
        let mut messages = project(prefix);
        place_trailing_cache_breakpoint(&mut messages);
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![
                ApiBlock::ToolResult {
                    tool_use_id: call.id.0.clone(),
                    content: peek_view,
                    is_error: raw.is_error,
                    cache_control: None,
                },
                ApiBlock::Text {
                    text: RETAIN_INSTRUCTION.to_string(),
                    cache_control: None,
                },
            ],
        });

        let verdict = match client
            .complete(&messages, &[retain_tool()], ToolChoice::force(RETAIN))
            .await
        {
            Ok(completion) => verdict_from(&completion),
            Err(_) => None,
        };

        match verdict {
            Some(v) => WireState::Distilled(render_verdict(lines, &v)),
            // Graceful default (DESIGN §4/§8): keep everything the peek saw rather
            // than drop data when the peek fails. Rendered as a canonical keep-all
            // hunk (not the per-line peek view) so the fallback stays lean too.
            None => {
                let note = (dropped > 0).then(|| format!("{dropped} further lines truncated"));
                WireState::Distilled(render_hunks(lines, &[(1, lines.len())], note.as_deref()))
            }
        }
    }
}

fn retain_tool() -> Value {
    json!({
        "name": RETAIN,
        "description": "Choose what of the tool output to keep on the record going forward. The output is line-numbered. Whatever you drop is gone from the working context, so keep anything that might matter — but stay lean.",
        "input_schema": {
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
        }
    })
}

// ---- the verdict ----

#[derive(Debug, PartialEq, Eq)]
enum Keep {
    All,
    None,
    Ranges(Vec<(usize, usize)>),
}

struct Verdict {
    keep: Keep,
    note: Option<String>,
}

#[derive(Deserialize)]
struct RetainInput {
    keep: String,
    #[serde(default)]
    note: Option<String>,
}

fn verdict_from(completion: &Completion) -> Option<Verdict> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let retain = calls.iter().find(|c| c.name == RETAIN)?;
    let RetainInput { keep, note } = RetainInput::deserialize(&retain.input).ok()?;
    Some(Verdict {
        keep: parse_keep(&keep),
        note,
    })
}

/// Parse a `keep` string: `all` / `none` / range list. Lenient — malformed
/// tokens are dropped; a list with no valid ranges reads as `none`.
fn parse_keep(s: &str) -> Keep {
    match s.trim().to_ascii_lowercase().as_str() {
        "all" => Keep::All,
        "none" | "" => Keep::None,
        _ => {
            let ranges: Vec<(usize, usize)> = s.split(',').filter_map(parse_range).collect();
            if ranges.is_empty() {
                Keep::None
            } else {
                Keep::Ranges(ranges)
            }
        }
    }
}

fn parse_range(tok: &str) -> Option<(usize, usize)> {
    let tok = tok.trim();
    if tok.is_empty() {
        return None;
    }
    match tok.split_once('-') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => {
            let v = tok.parse().ok()?;
            Some((v, v))
        }
    }
}

fn render_verdict(lines: &[&str], v: &Verdict) -> String {
    let n = lines.len();
    let ranges = match &v.keep {
        Keep::All => vec![(1, n)],
        Keep::None => vec![],
        Keep::Ranges(rs) => rs.clone(),
    };
    render_hunks(lines, &ranges, v.note.as_deref())
}

// ---- the renderers ----

/// Render every line `cat -n`-style (1-based, 6-wide, tab-separated). This is the
/// coordinate system the §4 peek addresses with `retain(lines=…)`, so it is shown
/// ONLY in the throwaway fork — never on the canonical wire, where a number on
/// every line proved heavy enough to make distillates *fatter* than the raw they
/// replace (measured across a live session). The canonical wire uses
/// [`render_hunks`] instead.
fn number_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render kept ranges for the **canonical wire** as diff-style hunks: a
/// `@@ lines a-b @@` header per contiguous span, then those lines *verbatim*
/// (unnumbered). The header carries the position anchor at one line per *range*
/// instead of a number on every *line*; gaps between hunks are implied by the
/// jump in the headers, exactly as in a unified diff. An optional trailing note
/// is the breadcrumb kept even when nothing is retained (DESIGN §4-realized) — it
/// must survive the empty-hunks case, so it is one overall note here, not
/// per-hunk.
fn render_hunks(lines: &[&str], ranges: &[(usize, usize)], note: Option<&str>) -> String {
    let ranges = normalize(ranges, lines.len());
    let mut parts: Vec<String> = Vec::new();
    for &(start, end) in &ranges {
        parts.push(hunk_header(start, end));
        for i in start..=end {
            parts.push(lines[i - 1].to_string());
        }
    }
    if let Some(note) = note.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("— {note}"));
    }
    if parts.is_empty() {
        parts.push("(nothing kept)".to_string());
    }
    parts.join("\n")
}

/// A unified-diff-style hunk header: `@@ lines a-b @@`, or `@@ line n @@` for a
/// single kept line.
fn hunk_header(start: usize, end: usize) -> String {
    if start == end {
        format!("@@ line {start} @@")
    } else {
        format!("@@ lines {start}-{end} @@")
    }
}

/// Clamp ranges to `[1, n]`, drop out-of-range, sort, and merge
/// overlapping/adjacent. Tolerates reversed ranges.
fn normalize(ranges: &[(usize, usize)], n: usize) -> Vec<(usize, usize)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Usage;
    use crate::log::CallId;

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    /// A `Completion` whose assistant turn made a single `retain` call carrying
    /// `input` — the shape `ForkPeek` forces and `verdict_from` parses.
    fn retain_completion(name: &str, input: Value) -> Completion {
        Completion {
            assistant: Payload::Assistant {
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("r".into()),
                    name: name.into(),
                    input,
                }],
            },
            model: "test".into(),
            stop_reason: None,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        }
    }

    #[test]
    fn peek_view_numbers_every_line() {
        // The throwaway fork's view — per-line numbers are the `retain` coordinate
        // system. (Only here; never on the canonical wire.)
        let ls = lines("alpha\nbeta\ngamma");
        let view = number_lines(&ls);
        assert!(view.contains("     1\talpha"), "{view}");
        assert!(view.contains("     3\tgamma"), "{view}");
    }

    #[test]
    fn canonical_hunk_is_verbatim_under_a_diff_header() {
        let ls = lines("alpha\nbeta\ngamma");
        let wire = render_hunks(&ls, &[(1, 3)], None);
        assert!(wire.contains("@@ lines 1-3 @@"), "{wire}");
        assert!(wire.lines().any(|l| l == "alpha") && wire.lines().any(|l| l == "gamma"), "{wire}");
        // The inflation fix: no per-line numbering on the canonical wire.
        assert!(!wire.contains("     1\talpha"), "{wire}");
    }

    #[test]
    fn gapped_hunks_get_a_header_each_and_drop_the_gap() {
        let body = (1..=100).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let ls = lines(&body);
        let wire = render_hunks(&ls, &[(1, 3), (99, 100)], None);
        assert!(wire.contains("@@ lines 1-3 @@"), "{wire}");
        assert!(wire.contains("@@ lines 99-100 @@"), "{wire}"); // absolute; gap implied
        assert!(wire.lines().any(|l| l == "line1") && wire.lines().any(|l| l == "line100"), "{wire}");
        assert!(!wire.contains("line50"), "middle dropped");
        assert!(wire.len() < body.len(), "leaner than raw");
    }

    #[test]
    fn single_line_hunk_uses_the_singular_header() {
        let ls = lines("a\nb\nc");
        let wire = render_hunks(&ls, &[(2, 2)], None);
        assert!(wire.contains("@@ line 2 @@"), "{wire}");
        assert!(wire.lines().any(|l| l == "b"));
    }

    #[test]
    fn note_appends_and_none_falls_back_to_breadcrumb() {
        let ls = lines("a\nb\nc");
        let kept = render_hunks(&ls, &[(2, 2)], Some("the b line"));
        assert!(kept.contains("@@ line 2 @@"), "{kept}");
        assert!(kept.ends_with("— the b line"), "{kept}");

        let nothing = render_hunks(&ls, &[], Some("wrong file"));
        assert_eq!(nothing, "— wrong file");

        let bare = render_hunks(&ls, &[], None);
        assert_eq!(bare, "(nothing kept)");
    }

    #[test]
    fn parse_keep_handles_keywords_ranges_and_garbage() {
        assert_eq!(parse_keep("all"), Keep::All);
        assert_eq!(parse_keep(" ALL "), Keep::All);
        assert_eq!(parse_keep("none"), Keep::None);
        assert_eq!(parse_keep(""), Keep::None);
        assert_eq!(parse_keep("7"), Keep::Ranges(vec![(7, 7)]));
        assert_eq!(
            parse_keep(" 40-52 , 310-318 "),
            Keep::Ranges(vec![(40, 52), (310, 318)])
        );
        assert_eq!(parse_keep("just words"), Keep::None); // no valid ranges
    }

    #[test]
    fn normalize_clamps_sorts_and_merges() {
        assert_eq!(normalize(&[(1, 3), (2, 5)], 100), vec![(1, 5)]); // overlap
        assert_eq!(normalize(&[(1, 3), (4, 6)], 100), vec![(1, 6)]); // adjacent
        assert_eq!(normalize(&[(310, 318), (40, 52)], 400), vec![(40, 52), (310, 318)]); // sort
        assert_eq!(normalize(&[(1, 1000)], 100), vec![(1, 100)]); // clamp high
        assert!(normalize(&[(0, 0)], 100).is_empty()); // line 0 is invalid → dropped
        assert!(normalize(&[(50, 60)], 10).is_empty()); // wholly out of range
    }

    #[test]
    fn verdict_from_parses_keep_and_optional_note() {
        let v = verdict_from(&retain_completion(RETAIN, json!({"keep": "all", "note": "the gist"})))
            .expect("a retain call parses");
        assert_eq!(v.keep, Keep::All);
        assert_eq!(v.note.as_deref(), Some("the gist"));

        // note is optional; ranges parse through to the verdict.
        let v = verdict_from(&retain_completion(RETAIN, json!({"keep": "40-52,310-318"})))
            .expect("ranges parse");
        assert_eq!(v.keep, Keep::Ranges(vec![(40, 52), (310, 318)]));
        assert_eq!(v.note, None);
    }

    #[test]
    fn verdict_from_is_none_when_no_retain_call() {
        // The peek is force-called to `retain`, but guard the parse anyway: a turn
        // that called something else (or nothing) yields no verdict → keep-all.
        assert!(verdict_from(&retain_completion("bash", json!({"command": "ls"}))).is_none());
    }

    #[test]
    fn render_verdict_covers_all_none_and_ranges() {
        let ls = lines("a\nb\nc\nd\ne");

        let all = render_verdict(&ls, &Verdict { keep: Keep::All, note: None });
        assert!(all.contains("@@ lines 1-5 @@"), "{all}");
        assert!(all.lines().any(|l| l == "a") && all.lines().any(|l| l == "e"), "{all}");

        // None keeps only the breadcrumb note — the inline dead-end shed (DESIGN §4).
        let none = render_verdict(
            &ls,
            &Verdict { keep: Keep::None, note: Some("nothing useful here".into()) },
        );
        assert_eq!(none, "— nothing useful here");

        let ranges = render_verdict(&ls, &Verdict { keep: Keep::Ranges(vec![(2, 3)]), note: None });
        assert!(ranges.contains("@@ lines 2-3 @@"), "{ranges}");
        assert!(ranges.lines().any(|l| l == "b") && ranges.lines().any(|l| l == "c"), "{ranges}");
        assert!(!ranges.contains("@@ lines 1") && !ranges.contains("@@ line 1 "), "line 1 dropped: {ranges}");
    }
}
