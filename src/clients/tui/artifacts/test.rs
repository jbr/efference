use super::*;
use crate::record::artifact::Kind;

#[test]
fn linked_fences_are_read_from_the_entry_view() {
    let view = serde_json::json!({
        "seq": 19,
        "payload": {"Assistant": {
            "output": {"Messages": []},
            "artifacts": [
                {"text_block": 0, "fence": 0, "kind": "html",
                 "url": "/artifacts/a/19/0/0/t"},
                {"text_block": 1, "fence": 2, "kind": "mermaid",
                 "url": "/artifacts/a/19/1/2/u"},
                // A fence whose kind the view does not name is not an artifact.
                {"text_block": 2, "fence": 0, "kind": "svg",
                 "url": "/artifacts/a/19/2/0/v"},
            ],
        }},
    });

    assert_eq!(
        linked_fences(&view),
        [
            (
                Coordinate {
                    seq: 19,
                    text_block: 0,
                    fence: 0
                },
                Kind::Html,
            ),
            (
                Coordinate {
                    seq: 19,
                    text_block: 1,
                    fence: 2
                },
                Kind::Mermaid,
            ),
        ]
    );
}

#[test]
fn entries_without_artifacts_have_no_links() {
    for view in [
        serde_json::json!({"seq": 3, "payload": {"User": [{"Text": "```html-artifact\n<p/>\n"}]}}),
        serde_json::json!({"seq": 4, "payload": {"Assistant": {"output": {"Messages": []}}}}),
    ] {
        assert!(linked_fences(&view).is_empty(), "{view}");
    }
}
