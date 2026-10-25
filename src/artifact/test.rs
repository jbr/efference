use super::*;

#[test]
fn counts_every_fenced_block_and_extracts_only_exact_closed_artifacts() {
    let markdown = concat!(
        "```rust\nlet ordinary = true;\n```\n",
        "\n",
        "~~~html-artifact\n<main>first</main>\n~~~\n",
        "\n",
        "```html-artifact preview\nnot an artifact\n```\n",
        "\n",
        "````html-artifact\n<main>second</main>\n````\n",
    );

    assert_eq!(
        closed_html_artifact_fences(markdown),
        vec![
            HtmlArtifactFence {
                ordinal: 1,
                html: "<main>first</main>\n".to_owned(),
            },
            HtmlArtifactFence {
                ordinal: 3,
                html: "<main>second</main>\n".to_owned(),
            },
        ]
    );
}

#[test]
fn ignores_unclosed_fences_even_when_the_parser_emits_a_code_block_end() {
    let markdown = "```html-artifact\n<main>unfinished</main>\n";
    assert!(closed_html_artifact_fences(markdown).is_empty());
}

#[test]
fn accepts_closed_fences_in_blockquotes_and_lists() {
    let markdown = concat!(
        "> ```html-artifact\n",
        "> <main>quoted</main>\n",
        "> ```\n",
        "\n",
        "- ```html-artifact\n",
        "  <main>listed</main>\n",
        "  ```\n",
    );

    assert_eq!(
        closed_html_artifact_fences(markdown),
        vec![
            HtmlArtifactFence {
                ordinal: 0,
                html: "<main>quoted</main>\n".to_owned(),
            },
            HtmlArtifactFence {
                ordinal: 1,
                html: "<main>listed</main>\n".to_owned(),
            },
        ]
    );
}

#[test]
fn indented_code_does_not_advance_fence_ordinals() {
    let markdown = concat!(
        "    ```html-artifact\n",
        "    <main>indented code</main>\n",
        "    ```\n",
        "\n",
        "```html-artifact\n<main>artifact</main>\n```\n",
    );

    assert_eq!(
        closed_html_artifact_fences(markdown),
        vec![HtmlArtifactFence {
            ordinal: 0,
            html: "<main>artifact</main>\n".to_owned(),
        }]
    );
}

#[test]
fn mismatched_or_too_short_closing_markers_do_not_close_a_fence() {
    let markdown = concat!(
        "````html-artifact\n<main>mismatched</main>\n",
        "```\n",
        "\n",
        "~~~html-artifact\n<main>wrong marker</main>\n",
        "```\n",
    );
    assert!(closed_html_artifact_fences(markdown).is_empty());
}

#[test]
fn container_prefix_lookalikes_in_fence_content_are_not_closers() {
    let markdowns = [
        "```html-artifact\n<main>quote lookalike</main>\n> ```\n",
        "```html-artifact\n<main>list lookalike</main>\n- ```\n",
        "```html-artifact\n<main>nested lookalike</main>\n> - > ```\n",
    ];

    for markdown in markdowns {
        assert!(
            closed_html_artifact_fences(markdown).is_empty(),
            "unexpected artifact from {markdown:?}"
        );
    }
}
