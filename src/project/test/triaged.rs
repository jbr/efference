//! Rendering a triaged verdict back to wire text (docs/triage.md).

use super::*;

/// A verdict for the renderer under test, from the three values a case varies.
fn verdict(keep: Keep, note: Option<&str>, intent_feedback: Option<&str>) -> Triaged {
    let mut v = Triaged::new(keep);
    if let Some(note) = note {
        v = v.with_note(note);
    }
    if let Some(fb) = intent_feedback {
        v = v.with_intent_feedback(fb);
    }
    v
}
use crate::project::render::normalize;

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// `render_hunks` over lines that were all shown to triage.
fn render_hunks(lines: &[&str], ranges: &[(usize, usize)], verdict: &Triaged) -> String {
    crate::project::render::render_hunks(lines, ranges, verdict, lines.len())
}

/// The canonical render over a stored verdict (docs/triage.md): `all` is the
/// content verbatim and *unheaded*, `none` collapses to the breadcrumb note, ranges keep
/// only their spans.
#[test]
fn render_triaged_covers_all_none_and_ranges() {
    let c = "a\nb\nc\nd\ne";
    let all = render_triaged(c, &verdict(Keep::All, None, None));
    assert_eq!(all, c, "a fully-retained result is the result: {all}");

    let none = render_triaged(c, &verdict(Keep::None, Some("nothing useful here"), None));
    assert_eq!(none, "(output dropped by triage)\n⟢ nothing useful here");

    let ranges = render_triaged(c, &verdict(Keep::Ranges(vec![(2, 3)]), None, None));
    assert!(ranges.contains("@@ lines 2-3 @@"), "{ranges}");
    assert!(
        ranges.lines().any(|l| l == "b") && ranges.lines().any(|l| l == "c"),
        "{ranges}"
    );
    assert!(
        !ranges.contains("@@ lines 1") && !ranges.contains("@@ line 1 "),
        "line 1 dropped: {ranges}"
    );
}

/// Past `TRIAGE_LINE_CAP` the render appends a truncation breadcrumb and a range
/// pointing into the truncated tail can't reach those lines (the verdict only ever
/// addressed the shown prefix).
#[test]
fn render_triaged_truncates_past_the_cap() {
    let body = (1..=TRIAGE_LINE_CAP + 5)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = render_triaged(&body, &verdict(Keep::Ranges(vec![(1, 1)]), None, None));
    assert!(out.lines().any(|l| l == "line1"), "kept line present");
    assert!(
        out.contains("5 more lines truncated"),
        "truncation noted: {out}"
    );

    let past = render_triaged(
        &body,
        &verdict(
            Keep::Ranges(vec![(TRIAGE_LINE_CAP + 2, TRIAGE_LINE_CAP + 4)]),
            None,
            None,
        ),
    );
    assert!(
        !past.contains(&format!("line{}", TRIAGE_LINE_CAP + 3)),
        "beyond-cap line unreachable: {past}"
    );

    // The closing marker's total includes the lines the cap dropped.
    let cut = render_triaged(&body, &verdict(Keep::Ranges(vec![(1, 10)]), None, None));
    let total = TRIAGE_LINE_CAP + 5;
    assert!(
        cut.ends_with(&format!(
            "@@ {total} lines total @@\n… [5 more lines truncated before triage]"
        )),
        "{cut}"
    );
}

/// A hunk header marks *where* curation cut, so a single range spanning the whole
/// result carries none: it would state only "all of it", and on a one-line result
/// the header is longer than the line it labels (docs/triage.md).
#[test]
fn a_range_spanning_everything_gets_no_header() {
    let ls = lines("alpha\nbeta\ngamma");
    let wire = render_hunks(&ls, &[(1, 3)], &verdict(Keep::All, None, None));
    assert_eq!(wire, "alpha\nbeta\ngamma", "{wire}");

    // The one-line case that made triage net-negative on short results.
    assert_eq!(
        render_hunks(&lines("done"), &[(1, 1)], &verdict(Keep::All, None, None)),
        "done"
    );

    // A note still rides along; it is the breadcrumb, not the position anchor.
    assert_eq!(
        render_hunks(
            &lines("done"),
            &[(1, 1)],
            &verdict(Keep::All, Some("wrote the file"), None)
        ),
        "done\n⟢ wrote the file"
    );
}

/// A result triaged to nothing opens with a fixed marker, with or without a note, so
/// the note is never the whole result.
#[test]
fn nothing_kept_opens_with_the_marker() {
    let ls = lines("a\nb\nc");
    assert_eq!(
        render_hunks(&ls, &[], &verdict(Keep::None, None, None)),
        "(output dropped by triage)"
    );
    assert_eq!(
        render_hunks(&ls, &[], &verdict(Keep::None, Some("a build log"), None)),
        "(output dropped by triage)\n⟢ a build log"
    );
    // Ranges that all fall outside the output keep nothing too.
    assert_eq!(
        render_hunks(&ls, &[(9, 10)], &verdict(Keep::None, None, None)),
        "(output dropped by triage)"
    );
    // An empty output had nothing to drop.
    assert_eq!(
        render_hunks(&[], &[], &verdict(Keep::None, None, None)),
        "(nothing kept)"
    );
}

/// The end of a cut result is marked: a range stopping short of the last line is
/// followed by the line total.
#[test]
fn a_cut_tail_closes_with_the_line_total() {
    let body = (1..=120)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let ls = lines(&body);
    let wire = render_hunks(&ls, &[(30, 40)], &verdict(Keep::All, Some("n"), None));
    assert!(wire.starts_with("@@ lines 30-40 @@\nline30\n"), "{wire}");
    assert!(
        wire.ends_with("line40\n@@ 120 lines total @@\n⟢ n"),
        "{wire}"
    );
    // A range that reaches the last line has no closing marker.
    let tail = render_hunks(&ls, &[(110, 120)], &verdict(Keep::All, None, None));
    assert!(!tail.contains("lines total"), "{tail}");
}

/// Trailing blank lines count as covered: the `read` frame ends in an empty line, and
/// a keep stopping before it cut nothing.
#[test]
fn trailing_blank_lines_count_as_covered() {
    let mut body = (1..=100)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    body.push_str("\n\n");
    let ls = lines(&body);
    assert_eq!(ls.len(), 101, "{body:?}");
    let wire = render_hunks(&ls, &[(1, 100)], &verdict(Keep::All, None, None));
    assert!(!wire.contains("@@"), "unheaded, no closing marker: {wire}");
    assert!(
        wire.starts_with("line1\n") && wire.ends_with("line100"),
        "{wire}"
    );

    // Content beyond the range still closes it.
    let cut = render_hunks(&ls, &[(1, 99)], &verdict(Keep::All, None, None));
    assert!(cut.ends_with("@@ 101 lines total @@"), "{cut}");
}

/// A partial range keeps its header — that is the case the marker exists for.
#[test]
fn a_partial_range_keeps_its_header() {
    let ls = lines("alpha\nbeta\ngamma");
    let wire = render_hunks(&ls, &[(1, 2)], &verdict(Keep::All, None, None));
    assert!(wire.contains("@@ lines 1-2 @@"), "{wire}");
    assert!(!wire.contains("gamma"), "{wire}");
    assert!(
        !wire.contains("     1\talpha"),
        "no per-line numbering on the wire: {wire}"
    );
}

#[test]
fn gapped_hunks_get_a_header_each_and_drop_the_gap() {
    let body = (1..=100)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let ls = lines(&body);
    let wire = render_hunks(&ls, &[(1, 3), (99, 100)], &verdict(Keep::All, None, None));
    assert!(
        wire.contains("@@ lines 1-3 @@") && wire.contains("@@ lines 99-100 @@"),
        "{wire}"
    );
    assert!(!wire.contains("line50"), "middle dropped");
    assert!(wire.len() < body.len(), "leaner than raw");
}

#[test]
fn single_line_hunk_uses_the_singular_header() {
    let ls = lines("a\nb\nc");
    let wire = render_hunks(&ls, &[(2, 2)], &verdict(Keep::All, None, None));
    assert!(wire.contains("@@ line 2 @@"), "{wire}");
    assert!(wire.lines().any(|l| l == "b"));
}

#[test]
fn note_appends_and_empty_falls_back_to_breadcrumb() {
    let ls = lines("a\nb\nc");
    assert!(
        render_hunks(
            &ls,
            &[(2, 2)],
            &verdict(Keep::All, Some("the b line"), None)
        )
        .ends_with("⟢ the b line")
    );
    assert_eq!(
        render_hunks(&ls, &[], &verdict(Keep::All, Some("wrong file"), None)),
        "(output dropped by triage)\n⟢ wrong file"
    );
    assert_eq!(
        render_hunks(&ls, &[], &verdict(Keep::All, None, None)),
        "(output dropped by triage)"
    );
}

#[test]
fn normalize_clamps_sorts_and_merges() {
    assert_eq!(normalize(&[(1, 3), (2, 5)], 100), vec![(1, 5)]);
    assert_eq!(normalize(&[(1, 3), (4, 6)], 100), vec![(1, 6)]);
    assert_eq!(
        normalize(&[(310, 318), (40, 52)], 400),
        vec![(40, 52), (310, 318)]
    );
    assert_eq!(normalize(&[(1, 1000)], 100), vec![(1, 100)]);
    assert!(normalize(&[(0, 0)], 100).is_empty());
    assert!(normalize(&[(50, 60)], 10).is_empty());
}

/// TriageOutcome projects the *verdict* re-rendered from `raw` (docs/triage.md): the
/// kept line and note land; the dropped lines never reach the wire.
#[test]
fn triaged_projects_the_verdict_not_raw() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("bash", "x", json!({}))],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            1,
            0,
            Payload::tool_result(
                "x",
                RawResult::text("the gist line\nten thousand lines of output"),
                WireState::from(Triaged::new(Keep::Ranges(vec![(1, 1)])).with_note("summary")),
            ),
        ),
    ];
    let wire = serde_json::to_string(&project(&log)).unwrap();
    assert!(
        wire.contains("the gist line"),
        "kept line projected: {wire}"
    );
    assert!(wire.contains("summary"), "note projected: {wire}");
    assert!(
        !wire.contains("ten thousand lines"),
        "dropped lines stay off the wire: {wire}"
    );
}

/// A [`WireState::TriageFailed`] result projects a **hard error in place of the
/// output** (docs/triage.md): the untriaged raw never reaches the wire,
/// `is_error` is forced true regardless of the tool's own outcome, and the message
/// carries the why plus the stable greppable prefix.
#[test]
fn triage_failed_projects_an_error_never_the_raw() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("bash", "x", json!({}))],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            1,
            0,
            Payload::tool_result(
                "x",
                RawResult::text("ten thousand lines of output"),
                WireState::TriageFailed {
                    why: "triage called no tool (attempt 3 of 3)".into(),
                },
            ),
        ),
    ];
    let messages = project(&log);
    let block = messages
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ApiBlock::ToolResult {
                content, is_error, ..
            } => Some((content, is_error)),
            _ => None,
        })
        .expect("the tool_use stays answered");
    let (content, is_error) = block;
    assert!(*is_error, "surfaced as a failed call: {content}");
    assert!(
        content
            .contains("result withheld — triage failed (triage called no tool (attempt 3 of 3))"),
        "stable prefix + why: {content}"
    );
    assert!(
        content.contains("ran to completion"),
        "the agent is told the call did run (side effects stand): {content}"
    );
    assert!(
        !content.contains("ten thousand lines"),
        "the untriaged raw never reaches the wire: {content}"
    );
}

/// `intent_feedback` renders after the note under the same `⟢` sign with a fixed lead,
/// so the agent reads it as a remark about the channel; blank feedback renders nothing
/// (docs/triage.md).
#[test]
fn intent_feedback_follows_the_note_under_its_own_lead() {
    let ls = lines("a\nb");
    assert_eq!(
        render_hunks(
            &ls,
            &[(1, 2)],
            &verdict(
                Keep::All,
                Some("two lines"),
                Some("no question to keep against")
            )
        ),
        "a\nb\n⟢ two lines\n⟢ re your intent: no question to keep against"
    );
    assert_eq!(
        render_hunks(
            &ls,
            &[],
            &verdict(Keep::All, None, Some("no question to keep against"))
        ),
        "(output dropped by triage)\n⟢ re your intent: no question to keep against"
    );
    assert_eq!(
        render_hunks(&ls, &[], &verdict(Keep::All, None, Some("  "))),
        "(output dropped by triage)"
    );
}

/// A `Triaged` state written before `intent_feedback` existed deserializes with it
/// absent, and an absent field is not written — recorded logs and their bytes are
/// unchanged by the field's arrival (docs/log-and-projection.md).
#[test]
fn intent_feedback_is_absent_from_older_states_and_omitted_when_none() {
    let old = r#"{"Triaged":{"keep":"All","note":"n"}}"#;
    let wire: WireState = serde_json::from_str(old).unwrap();
    assert!(matches!(
        &wire,
        WireState::Triaged(Triaged {
            intent_feedback: None,
            ..
        })
    ));
    assert_eq!(serde_json::to_string(&wire).unwrap(), old);
    let with = WireState::from(Triaged::new(Keep::All).with_intent_feedback("fb"));
    assert!(
        serde_json::to_string(&with)
            .unwrap()
            .contains("\"intent_feedback\":\"fb\"")
    );
}
