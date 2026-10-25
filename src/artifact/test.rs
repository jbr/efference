use super::*;

/// The cases the web client's fence numbering is tested against too: both sides must
/// find the same fences, in the same order, with the same source.
#[test]
fn the_shared_fence_cases_match() {
    #[derive(serde::Deserialize)]
    struct Case {
        case: String,
        markdown: String,
        fences: Vec<String>,
    }
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../web/src/ui/artifact-fences.json")).unwrap();
    assert!(!cases.is_empty());
    for Case {
        case,
        markdown,
        fences,
    } in cases
    {
        // remark drops the code block's final line ending; pulldown-cmark keeps it.
        let found: Vec<String> = html_artifact_fences(&markdown)
            .into_iter()
            .map(|html| html.strip_suffix('\n').unwrap_or(&html).to_owned())
            .collect();
        assert_eq!(found, fences, "{case}");
    }
}

#[test]
fn text_without_the_marker_has_no_fences() {
    assert!(html_artifact_fences("```html\n<p>source</p>\n```\n").is_empty());
}
