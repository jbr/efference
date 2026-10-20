//! The triage view's margin numbering and the `keep` ranges that address it (docs/triage.md).
use super::*;

fn view_of(content: &str) -> String {
    result_view(
        "r1",
        &ToolCall::new("read", "read_1", json!({})),
        &RawResult::text(content),
    )
}

/// The embedded-number shape that drew the out-of-range retries: a `read` result whose
/// lines carry the file's own line numbers.
const READ_OUTPUT: &str = "286\tfn a() {\n287\t    b();\n288\t}";

#[test]
fn view_numbers_lines_in_a_margin_and_names_it() {
    let v = view_of(READ_OUTPUT);
    assert_eq!(
        v,
        "═══ result r1 · read ═══\n\
         Each line starts with its line number and a │ margin (`12 │ text`); numbers after \
         the margin are part of the text, and `keep` uses the margin's numbers.\n\
         \x20  1 │ 286\tfn a() {\n\
         \x20  2 │ 287\t    b();\n\
         \x20  3 │ 288\t}"
    );
}

#[test]
fn one_line_result_is_numbered_from_one() {
    let v = view_of("hello");
    assert!(v.ends_with("\n   1 │ hello"), "{v}");
    assert!(!v.contains("   2 │"), "{v}");
}

#[test]
fn truncation_marker_sits_under_the_margin() {
    let content = (0..TRIAGE_LINE_CAP + 3)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let v = view_of(&content);
    assert!(v.ends_with("\n     … [3 more lines truncated] …"), "{v}");
}

fn verdict(keep: &str) -> ParsedRetain {
    let offered = [CALL_ID.to_string()];
    parse_retain(
        &retain_completion(RETAIN, json!({ "keep": keep })),
        &offered,
    )
    .unwrap()
}

#[test]
fn a_range_in_margin_numbers_is_accepted() {
    let shown = [(CALL_ID.to_string(), 3)];
    let ok = require_in_bounds(verdict("2-3"), &shown).unwrap();
    assert_eq!(ok.results[0].keep, Keep::Ranges(vec![(2, 3)]));
}

#[test]
fn a_range_in_the_outputs_own_numbers_is_refused() {
    let shown = [(CALL_ID.to_string(), 3)];
    let Err(why) = require_in_bounds(verdict("287-288"), &shown) else {
        panic!("a range past the shown lines must be refused");
    };
    assert!(
        why.contains("beyond the shown output, which has 3 lines"),
        "{why}"
    );
}
