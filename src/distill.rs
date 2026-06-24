//! Distillation — what a raw tool result projects to on the canonical wire
//! (DESIGN §4-realized, §10 Phase 1).
//!
//! The thesis: the durable wire **never carries raw output**. Every tool result
//! is distilled the moment it's appended — so a result's `wire` is fixed at
//! append and never mutated, which is exactly why caching stays safe (no cached
//! prefix ever changes; redaction is an append-time decision, not a later scrub).
//!
//! The distiller is the pluggable policy (§5, "the most-iterated knob") behind a
//! trait. Slice 1 ships a deterministic stub, [`HeadTail`], standing in for the
//! eventual model peek ([`ForkPeek`], slice 2 / Phase 2) so the wire-divergence
//! plumbing is provable in isolation. The loop never changes when we swap.
//!
//! Output is `cat -n`-style **line-numbered** (DESIGN §4): raw stays pristine in
//! the log; numbering is the render transform, and the distillate keeps the
//! *original, absolute* numbers across an elision so any kept line maps back to
//! raw — the coordinate system `retain(lines=…)` will address in slice 2.

use crate::log::{RawResult, WireState};

/// Decides the wire form of a raw tool result. Canonical carries only the result;
/// `raw` lives in the log untouched (DESIGN §4).
pub trait Distiller {
    fn distill(&self, raw: &RawResult) -> WireState;
}

/// Slice-1 stub for the model peek: keep the first `head` + last `tail` lines,
/// line-numbered, with an elision marker between; small results pass through
/// whole. Deterministic — purely a stand-in until `ForkPeek` (DESIGN §10).
pub struct HeadTail {
    pub head: usize,
    pub tail: usize,
}

impl Distiller for HeadTail {
    fn distill(&self, raw: &RawResult) -> WireState {
        let lines: Vec<&str> = raw.content.lines().collect();
        let n = lines.len();
        if n <= self.head + self.tail {
            // Small enough to keep whole — still line-numbered.
            return WireState::Distilled(number(&lines, 0, n));
        }
        let elided = n - self.head - self.tail;
        let head = number(&lines, 0, self.head);
        let tail = number(&lines, n - self.tail, n);
        WireState::Distilled(format!(
            "{head}\n       … [{elided} lines elided — raw in log] …\n{tail}"
        ))
    }
}

/// `cat -n`-style numbering of `lines[start..end]`: 1-based and **absolute**, so
/// numbers survive an elision and a kept line maps back to its raw position.
fn number(lines: &[&str], start: usize, end: usize) -> String {
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(content: &str) -> RawResult {
        RawResult {
            content: content.into(),
            is_error: false,
        }
    }

    fn distilled(state: WireState) -> String {
        match state {
            WireState::Distilled(s) => s,
            other => panic!("expected Distilled, got {other:?}"),
        }
    }

    #[test]
    fn small_output_kept_whole_and_line_numbered() {
        let wire = distilled(HeadTail { head: 20, tail: 10 }.distill(&raw("alpha\nbeta\ngamma")));
        assert!(wire.contains("     1\talpha"), "{wire}");
        assert!(wire.contains("     3\tgamma"), "{wire}");
        assert!(!wire.contains("elided"));
    }

    #[test]
    fn large_output_trimmed_with_absolute_line_numbers() {
        let content = (1..=100)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let wire = distilled(HeadTail { head: 3, tail: 2 }.distill(&raw(&content)));

        assert!(wire.contains("     1\tline1"), "{wire}"); // head
        assert!(wire.contains("     3\tline3"), "{wire}");
        assert!(wire.contains("[95 lines elided"), "{wire}"); // right count
        assert!(wire.contains("    99\tline99"), "{wire}"); // tail keeps ABSOLUTE numbers
        assert!(wire.contains("   100\tline100"), "{wire}");
        assert!(!wire.contains("line50"), "middle dropped"); // genuinely trimmed
        assert!(wire.len() < content.len(), "materially leaner than raw");
    }

    #[test]
    fn distilling_does_not_touch_raw() {
        let r = raw("a\nb\nc\nd");
        let _ = HeadTail { head: 1, tail: 1 }.distill(&r);
        assert_eq!(r.content, "a\nb\nc\nd", "raw is borrowed, never mutated");
    }
}
