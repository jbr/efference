//! Rendering a distilled verdict back to wire text (docs/redaction-fork.md).

use super::*;

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// The canonical render over a stored verdict (docs/redaction-fork.md): `all` is one verbatim
/// hunk, `none` collapses to the breadcrumb note, ranges keep only their spans.
#[test]
fn render_distilled_covers_all_none_and_ranges() {
    let c = "a\nb\nc\nd\ne";
    let all = render_distilled(c, &Keep::All, None);
    assert!(all.contains("@@ lines 1-5 @@"), "{all}");
    assert!(
        all.lines().any(|l| l == "a") && all.lines().any(|l| l == "e"),
        "{all}"
    );

    let none = render_distilled(c, &Keep::None, Some("nothing useful here"));
    assert_eq!(none, "⟢ nothing useful here");

    let ranges = render_distilled(c, &Keep::Ranges(vec![(2, 3)]), None);
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

/// Past `PEEK_LINE_CAP` the render appends a truncation breadcrumb and a range
/// pointing into the truncated tail can't reach those lines (the verdict only ever
/// addressed the shown prefix).
#[test]
fn render_distilled_truncates_past_the_cap() {
    let body = (1..=PEEK_LINE_CAP + 5)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = render_distilled(&body, &Keep::Ranges(vec![(1, 1)]), None);
    assert!(out.lines().any(|l| l == "line1"), "kept line present");
    assert!(
        out.contains("5 more lines truncated"),
        "truncation noted: {out}"
    );

    let past = render_distilled(
        &body,
        &Keep::Ranges(vec![(PEEK_LINE_CAP + 2, PEEK_LINE_CAP + 4)]),
        None,
    );
    assert!(
        !past.contains(&format!("line{}", PEEK_LINE_CAP + 3)),
        "beyond-cap line unreachable: {past}"
    );
}

#[test]
fn canonical_hunk_is_verbatim_under_a_diff_header() {
    let ls = lines("alpha\nbeta\ngamma");
    let wire = render_hunks(&ls, &[(1, 3)], None);
    assert!(wire.contains("@@ lines 1-3 @@"), "{wire}");
    assert!(
        wire.lines().any(|l| l == "alpha") && wire.lines().any(|l| l == "gamma"),
        "{wire}"
    );
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
    let wire = render_hunks(&ls, &[(1, 3), (99, 100)], None);
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
    let wire = render_hunks(&ls, &[(2, 2)], None);
    assert!(wire.contains("@@ line 2 @@"), "{wire}");
    assert!(wire.lines().any(|l| l == "b"));
}

#[test]
fn note_appends_and_empty_falls_back_to_breadcrumb() {
    let ls = lines("a\nb\nc");
    assert!(render_hunks(&ls, &[(2, 2)], Some("the b line")).ends_with("⟢ the b line"));
    assert_eq!(render_hunks(&ls, &[], Some("wrong file")), "⟢ wrong file");
    assert_eq!(render_hunks(&ls, &[], None), "(nothing kept)");
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

/// Distillation projects the *verdict* re-rendered from `raw` (docs/redaction-fork.md): the
/// kept line and note land; the dropped lines never reach the wire.
#[test]
fn distilled_projects_the_verdict_not_raw() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("x".into()),
                    name: "bash".into(),
                    input: json!({}),
                }],
            },
        ),
        entry(
            1,
            0,
            Payload::ToolResult {
                call_id: CallId("x".into()),
                raw: RawResult {
                    content: "the gist line\nten thousand lines of output".into(),
                    is_error: false,
                },
                wire: WireState::Distilled {
                    keep: Keep::Ranges(vec![(1, 1)]),
                    note: Some("summary".into()),
                },
            },
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
