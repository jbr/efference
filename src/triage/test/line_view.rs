//! The line-numbering arms of the triage view and their `keep` forms (docs/triage.md).
use super::*;

fn call_and_raw(content: &str) -> (ToolCall, RawResult) {
    (
        ToolCall::new("read", "read_1", json!({})),
        RawResult::text(content),
    )
}

/// The embedded-number shape that draws the out-of-range retries: a `read` result whose
/// lines carry the file's own line numbers.
const READ_OUTPUT: &str = "286\tfn a() {\n287\t    b();\n288\t}";

fn view_under(arm: LineView) -> String {
    let (call, raw) = call_and_raw(READ_OUTPUT);
    let mut view = format!("═══ result r1 · {} ═══\n", call.name);
    if let Some(s) = arm.header_sentence() {
        view.push_str(&format!("{s}\n"));
    }
    view.push_str(&arm.render(&lines(&raw.content)));
    view
}

#[test]
fn numbers_is_the_shipped_view_and_has_no_sentence() {
    let (call, raw) = call_and_raw(READ_OUTPUT);
    let shipped = result_view("r1", &call, &raw);
    assert!(shipped.contains("     2\t287\t    b();"), "{shipped}");
    assert_eq!(LineView::Numbers.header_sentence(), None);
}

#[test]
fn header_arm_keeps_the_numbers_and_adds_the_sentence() {
    let v = view_under(LineView::Header);
    assert!(
        v.contains("numbers inside the output are part of the text"),
        "{v}"
    );
    assert!(v.contains("     2\t287\t    b();"), "{v}");
}

#[test]
fn margin_arm_renders_a_glyph_margin_and_names_it() {
    let v = view_under(LineView::Margin);
    assert!(v.contains("\n   1 │ 286\tfn a() {"), "{v}");
    assert!(v.contains("\n   3 │ 288\t}"), "{v}");
    assert!(v.contains("│ margin"), "header names the margin: {v}");
}

#[test]
fn tag_arm_renders_a_unique_tag_per_line_not_a_count() {
    let v = view_under(LineView::Tags);
    let tags: Vec<String> = (1..=3).map(tag).collect();
    for (t, l) in tags.iter().zip(READ_OUTPUT.lines()) {
        assert!(v.contains(&format!("\n{t} │ {l}")), "{t}: {v}");
        assert_eq!(t.len(), 3);
        assert!(t.chars().next().unwrap().is_ascii_lowercase(), "{t}");
    }
    // Unique across everything a view can show, and stable (a pure function of the line).
    let all: std::collections::HashSet<String> = (1..=TRIAGE_LINE_CAP).map(tag).collect();
    assert_eq!(all.len(), TRIAGE_LINE_CAP);
    assert_eq!(tag(7), tag(7));
    assert!(v.contains("names ranges by the margin's tags"), "{v}");
}

#[test]
fn tags_round_trip_to_indices() {
    for n in [1, 2, 57, 1000, TRIAGE_LINE_CAP] {
        assert_eq!(untag(&tag(n)), Some(n));
    }
    assert_eq!(untag(&tag(5).to_uppercase()), Some(5));
    assert_eq!(untag("12"), None);
}

#[test]
fn tag_keep_maps_back_to_shown_line_indices() {
    let keep = format!("{}-{}, {}", tag(2), tag(9), tag(14));
    assert_eq!(
        LineView::Tags.parse_keep(&keep),
        Keep::Ranges(vec![(2, 9), (14, 14)])
    );
    assert_eq!(LineView::Tags.parse_keep("all"), Keep::All);
    assert_eq!(LineView::Tags.parse_keep("none"), Keep::None);
    // The numbered arms keep reading numbers.
    assert_eq!(
        LineView::Margin.parse_keep("40-52"),
        Keep::Ranges(vec![(40, 52)])
    );
}

fn tag_verdict(keep: &str) -> ParsedRetain {
    let offered = [CALL_ID.to_string()];
    let completion = retain_completion(RETAIN, json!({ "keep": keep }));
    // `parse_retain` reads the process env; build the same verdict under the tag arm.
    let mut p = parse_retain(&completion, &offered).unwrap();
    for v in &mut p.results {
        v.keep = LineView::Tags.parse_keep(&v.raw_keep);
    }
    p
}

#[test]
fn tag_validation_accepts_in_bounds_ranges() {
    let shown = [(CALL_ID.to_string(), 17)];
    let p = tag_verdict(&format!("{}-{}", tag(3), tag(17)));
    let ok = require_in_bounds_for(p, &shown, LineView::Tags).unwrap();
    assert_eq!(ok.results[0].keep, Keep::Ranges(vec![(3, 17)]));
}

#[test]
fn tag_validation_rejects_a_tag_beyond_the_shown_lines() {
    let shown = [(CALL_ID.to_string(), 17)];
    let p = tag_verdict(&format!("{}-{}", tag(30), tag(40)));
    let Err(why) = require_in_bounds_for(p, &shown, LineView::Tags) else {
        panic!("a tag past the shown output must be rejected");
    };
    assert!(why.contains("beyond the shown output"), "{why}");
    assert!(why.contains("17"), "names the bound: {why}");
}

#[test]
fn tag_validation_rejects_a_bogus_tag_and_embedded_numbers() {
    let shown = [(CALL_ID.to_string(), 17)];
    for keep in ["q9!-r2x", "75-88", &format!("{}-nope", tag(2))] {
        let p = tag_verdict(keep);
        let Err(why) = require_in_bounds_for(p, &shown, LineView::Tags) else {
            panic!("{keep:?} must be rejected, not dropped");
        };
        assert!(why.contains("names no line tag"), "{keep}: {why}");
    }
    // Keywords carry no tags.
    for keep in ["all", "none"] {
        assert!(require_in_bounds_for(tag_verdict(keep), &shown, LineView::Tags).is_ok());
    }
}

#[test]
fn tag_arm_rewrites_the_ask_and_the_schema_wording() {
    for fields in [KEEP_FIELDS, REPORT_FIELDS] {
        let ask = LineView::Tags.rewrite(fields);
        assert!(ask.contains("ranges of line tags like"), "{ask}");
        assert!(!ask.contains("1-based"), "{ask}");
    }
    let schema = LineView::Tags.rewrite(
        "comma-separated 1-based inclusive line ranges, e.g. \"40-52,310-318\" (a single line is \"7\")",
    );
    assert!(schema.contains("line tags"), "{schema}");
    assert_eq!(LineView::Margin.rewrite(KEEP_FIELDS), KEEP_FIELDS);
}
