use super::*;
use crate::log::{CallId, Keep, RawResult, Timestamp};

/// A tool result of `lines` numbered lines, under the given verdict.
fn result(seq: u64, lines: usize, wire: WireState) -> LogEntry {
    let content = (1..=lines)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(1000 + seq),
        payload: Payload::ToolResult {
            call_id: CallId(format!("call_{seq}")),
            raw: RawResult {
                content,
                is_error: false,
            },
            wire,
        },
    }
}

fn distilled(keep: Keep) -> WireState {
    WireState::Distilled { keep, note: None }
}

#[test]
fn nothing_folded_has_no_ratio() {
    // Zero shown is *unknown*, not 0% — the sidebar renders it as absent.
    assert_eq!(Retention::default().ratio(), None);
}

#[test]
fn ranges_sum_across_results() {
    let mut r = Retention::default();
    r.fold_all(&[
        result(0, 100, distilled(Keep::Ranges(vec![(1, 10), (91, 100)]))),
        result(1, 100, distilled(Keep::All)),
    ]);
    assert_eq!(r.kept_lines, 120);
    assert_eq!(r.shown_lines, 200);
    assert_eq!(r.ratio(), Some(0.6));
}

#[test]
fn kept_lines_match_the_rendered_distillate() {
    // The one invariant worth pinning: this must not become a second definition of
    // "kept" that drifts from what the transcript's gutter shows.
    let entry = result(0, 50, distilled(Keep::Ranges(vec![(5, 9), (20, 20)])));
    let Payload::ToolResult { raw, wire, .. } = &entry.payload else {
        unreachable!()
    };
    let WireState::Distilled { keep, .. } = wire else {
        unreachable!()
    };
    let mut r = Retention::default();
    r.fold(&entry);
    assert_eq!(
        r.kept_lines as usize,
        distillate(&raw.content, keep).numbered_lines().len()
    );
}

#[test]
fn overlapping_ranges_are_not_double_counted() {
    // The verdict is normalized before counting, as it is before rendering — a model
    // that writes `1-10,5-15` kept 15 lines, not 25.
    let mut r = Retention::default();
    r.fold(&result(0, 100, distilled(Keep::Ranges(vec![(1, 10), (5, 15)]))));
    assert_eq!(r.kept_lines, 15);
}

#[test]
fn a_withheld_result_is_counted_apart_from_the_ratio() {
    // A failed gate must not read as excellent compression.
    let mut r = Retention::default();
    r.fold_all(&[
        result(0, 100, distilled(Keep::Ranges(vec![(1, 50)]))),
        result(
            1,
            900,
            WireState::TriageFailed {
                why: "transport".into(),
            },
        ),
    ]);
    assert_eq!(r.withheld, 1);
    assert_eq!((r.kept_lines, r.shown_lines), (50, 100));
    assert_eq!(r.ratio(), Some(0.5));
}

#[test]
fn untriaged_and_elided_results_contribute_nothing() {
    let mut r = Retention::default();
    r.fold_all(&[
        result(0, 100, WireState::Raw),
        result(1, 100, WireState::Elided),
    ]);
    assert_eq!(r, Retention::default());
}

#[test]
fn lines_past_the_cap_are_truncated_not_dropped_silently() {
    // Beyond the cap the lines were never offered, so they belong to neither side of
    // the ratio — but they are reported, not silently absent.
    let mut r = Retention::default();
    r.fold(&result(0, super::super::PEEK_LINE_CAP + 25, distilled(Keep::All)));
    assert_eq!(r.shown_lines as usize, super::super::PEEK_LINE_CAP);
    assert_eq!(r.kept_lines as usize, super::super::PEEK_LINE_CAP);
    assert_eq!(r.truncated_lines, 25);
    assert_eq!(r.ratio(), Some(1.0));
}

#[test]
fn folding_incrementally_equals_folding_the_whole_span() {
    // What lets the daemon keep a cursor instead of rewalking the log each poll.
    let log = [
        result(0, 40, distilled(Keep::Ranges(vec![(1, 10)]))),
        result(1, 60, distilled(Keep::None)),
        result(
            2,
            10,
            WireState::TriageFailed {
                why: "retries exhausted".into(),
            },
        ),
        result(3, 80, distilled(Keep::All)),
    ];
    let mut whole = Retention::default();
    whole.fold_all(&log);

    let mut incremental = Retention::default();
    for entry in &log {
        incremental.fold(entry);
    }
    assert_eq!(whole, incremental);
}
