use super::*;
use crate::log::{Keep, RawResult, Timestamp, Triaged};

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
        submission_id: None,
        source: None,
        payload: Payload::tool_result(format!("call_{seq}"), RawResult::text(content), wire),
    }
}

/// The two shapes of one session's log (docs/triage.md): a result whose
/// verdict rides its own entry — every log written before capture existed, and every
/// fixture — and a result captured `Pending` with the verdict appended after it. Which
/// one wrote the session is not something the ratio may depend on.
#[test]
fn a_captured_result_counts_as_the_same_result_logged_with_its_verdict() {
    let verdict = WireState::from(Triaged::new(Keep::Ranges(vec![(1, 10), (91, 100)])));
    let decided = [result(0, 100, verdict.clone())];

    let mut direct = Retention::default();
    direct.fold_all(&decided, &decided);

    let captured = [
        result(0, 100, WireState::Pending),
        verdict_for(1, 0, verdict),
    ];

    let mut deferred = Retention::default();
    deferred.fold_all(&captured, &captured);

    assert_eq!(direct, deferred);
    assert_eq!((direct.kept_lines, direct.shown_lines), (20, 100));
}

/// The verdict decides a captured result; one naming a result already logged with a
/// final wire is not a second decision, and does not count again.
#[test]
fn a_verdict_for_an_already_decided_result_does_not_count_twice() {
    let verdict = WireState::from(Triaged::new(Keep::Ranges(vec![(1, 10)])));
    let log = [result(0, 100, verdict.clone()), verdict_for(1, 0, verdict)];

    let mut r = Retention::default();
    r.fold_all(&log, &log);

    assert_eq!((r.kept_lines, r.shown_lines), (10, 100));
}

/// A result whose verdict is still in flight counts as nothing yet: no ratio is
/// better than one built on an undecided line count.
#[test]
fn a_result_awaiting_its_verdict_counts_as_nothing() {
    let log = [result(0, 100, WireState::Pending)];
    let mut r = Retention::default();
    r.fold_all(&log, &log);
    assert_eq!(r, Retention::default());
    assert_eq!(r.ratio(), None);
}

/// The `ResultVerdict` a captured result waits for (docs/triage.md).
fn verdict_for(seq: u64, for_result: u64, wire: WireState) -> LogEntry {
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(1000 + seq),
        submission_id: None,
        source: None,
        payload: Payload::ResultVerdict { for_result, wire },
    }
}

#[test]
fn nothing_folded_has_no_ratio() {
    // Zero shown is *unknown*, not 0% — the sidebar renders it as absent.
    assert_eq!(Retention::default().ratio(), None);
}

#[test]
fn ranges_sum_across_results() {
    let mut r = Retention::default();
    let log = [
        result(
            0,
            100,
            WireState::from(Triaged::new(Keep::Ranges(vec![(1, 10), (91, 100)]))),
        ),
        result(1, 100, WireState::from(Triaged::new(Keep::All))),
    ];
    r.fold_all(&log, &log);
    assert_eq!(r.kept_lines, 120);
    assert_eq!(r.shown_lines, 200);
    assert_eq!(r.ratio(), Some(0.6));
}

#[test]
fn kept_lines_match_the_rendered_selection() {
    // The one invariant worth pinning: this must not become a second definition of
    // "kept" that drifts from what the transcript's gutter shows.
    let entry = result(
        0,
        50,
        WireState::from(Triaged::new(Keep::Ranges(vec![(5, 9), (20, 20)]))),
    );
    let Payload::ToolResult { raw, wire, .. } = &entry.payload else {
        unreachable!()
    };
    let WireState::Triaged(Triaged { keep, .. }) = wire else {
        unreachable!()
    };
    let mut r = Retention::default();
    r.fold(&entry, std::slice::from_ref(&entry));
    assert_eq!(
        r.kept_lines as usize,
        selection(&raw.content, keep).numbered_lines().len()
    );
}

#[test]
fn overlapping_ranges_are_not_double_counted() {
    // The verdict is normalized before counting, as it is before rendering — a model
    // that writes `1-10,5-15` kept 15 lines, not 25.
    let mut r = Retention::default();
    let entry = result(
        0,
        100,
        WireState::from(Triaged::new(Keep::Ranges(vec![(1, 10), (5, 15)]))),
    );
    r.fold(&entry, std::slice::from_ref(&entry));
    assert_eq!(r.kept_lines, 15);
}

#[test]
fn a_withheld_result_is_counted_apart_from_the_ratio() {
    // A failed gate must not read as excellent compression.
    let mut r = Retention::default();
    let log = [
        result(
            0,
            100,
            WireState::from(Triaged::new(Keep::Ranges(vec![(1, 50)]))),
        ),
        result(
            1,
            900,
            WireState::TriageFailed {
                why: "transport".into(),
            },
        ),
    ];
    r.fold_all(&log, &log);
    assert_eq!(r.withheld, 1);
    assert_eq!((r.kept_lines, r.shown_lines), (50, 100));
    assert_eq!(r.ratio(), Some(0.5));
}

#[test]
fn untriaged_and_elided_results_contribute_nothing() {
    let mut r = Retention::default();
    let log = [
        result(0, 100, WireState::Raw),
        result(1, 100, WireState::Elided),
    ];
    r.fold_all(&log, &log);
    assert_eq!(r, Retention::default());
}

#[test]
fn lines_past_the_cap_are_truncated_not_dropped_silently() {
    // Beyond the cap the lines were never offered, so they belong to neither side of
    // the ratio — but they are reported, not silently absent.
    let mut r = Retention::default();
    let entry = result(
        0,
        super::super::TRIAGE_LINE_CAP + 25,
        WireState::from(Triaged::new(Keep::All)),
    );
    r.fold(&entry, std::slice::from_ref(&entry));
    assert_eq!(r.shown_lines as usize, super::super::TRIAGE_LINE_CAP);
    assert_eq!(r.kept_lines as usize, super::super::TRIAGE_LINE_CAP);
    assert_eq!(r.truncated_lines, 25);
    assert_eq!(r.ratio(), Some(1.0));
}

#[test]
fn folding_incrementally_equals_folding_the_whole_span() {
    // What lets the daemon keep a cursor instead of rewalking the log each poll.
    let log = [
        result(
            0,
            40,
            WireState::from(Triaged::new(Keep::Ranges(vec![(1, 10)]))),
        ),
        result(1, 60, WireState::from(Triaged::new(Keep::None))),
        result(
            2,
            10,
            WireState::TriageFailed {
                why: "retries exhausted".into(),
            },
        ),
        result(3, 80, WireState::from(Triaged::new(Keep::All))),
    ];
    let mut whole = Retention::default();
    whole.fold_all(&log, &log);

    let mut incremental = Retention::default();
    for entry in &log {
        incremental.fold(entry, &log);
    }
    assert_eq!(whole, incremental);
}
