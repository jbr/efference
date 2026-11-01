use super::*;

/// The cases the web client's fence numbering is tested against too: both sides must
/// find the same fences, in the same order, with the same kind and source.
#[test]
fn the_shared_fence_cases_match() {
    #[derive(serde::Deserialize)]
    struct Case {
        case: String,
        markdown: String,
        fences: Vec<Expected>,
    }
    #[derive(serde::Deserialize)]
    struct Expected {
        kind: String,
        source: String,
    }
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../../web/src/ui/artifact-fences.json")).unwrap();
    assert!(!cases.is_empty());
    for Case {
        case,
        markdown,
        fences: expected,
    } in cases
    {
        // remark drops the code block's final line ending; pulldown-cmark keeps it.
        let found: Vec<(String, String)> = fences(&markdown)
            .into_iter()
            .map(|Fence { kind, source }| {
                (
                    serde_json::to_value(kind)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    source.strip_suffix('\n').unwrap_or(&source).to_owned(),
                )
            })
            .collect();
        let expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|Expected { kind, source }| (kind, source))
            .collect();
        assert_eq!(found, expected, "{case}");
    }
}

#[test]
fn text_without_a_marker_has_no_fences() {
    assert!(fences("```html\n<p>source</p>\n```\n").is_empty());
}
