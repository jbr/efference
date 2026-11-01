use super::*;

#[test]
fn loopback_links_are_read_from_the_entry_view() {
    let view = serde_json::json!({
        "seq": 19,
        "payload": {"Assistant": {
            "output": {"Messages": []},
            "artifacts": [
                {"text_block": 0, "fence": 0, "url": "/artifacts/a/19/0/0/t",
                 "external_url": "http://127.0.0.1:4321/artifacts/a/19/0/0/t"},
                {"text_block": 1, "fence": 2, "url": "/artifacts/a/19/1/2/u",
                 "external_url": "http://127.0.0.1:4321/artifacts/a/19/1/2/u"},
                // Outside a host there is no listener, so nothing to open.
                {"text_block": 2, "fence": 0, "url": "/artifacts/a/19/2/0/v"},
            ],
        }},
    });

    assert_eq!(
        external_links(&view),
        [
            (
                Coordinate {
                    seq: 19,
                    text_block: 0,
                    fence: 0
                },
                "http://127.0.0.1:4321/artifacts/a/19/0/0/t".to_owned()
            ),
            (
                Coordinate {
                    seq: 19,
                    text_block: 1,
                    fence: 2
                },
                "http://127.0.0.1:4321/artifacts/a/19/1/2/u".to_owned()
            ),
        ]
    );
}

#[test]
fn entries_without_artifacts_have_no_links() {
    for view in [
        serde_json::json!({"seq": 3, "payload": {"User": [{"Text": "```html-artifact\n<p/>\n```"}]}}),
        serde_json::json!({"seq": 4, "payload": {"Assistant": {"output": {"Messages": []}}}}),
    ] {
        assert!(external_links(&view).is_empty(), "{view}");
    }
}
