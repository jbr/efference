
use super::*;

#[test]
fn test_basic_markdown() {
    let input = "This is **bold** and this is *italic*.";
    let nodes = MarkdownRenderer::render(input);
    assert!(!nodes.is_empty());
    // Should contain spans with Strong and Emphasis styles
}

#[test]
fn test_code_block() {
    let input = "```rust\nfn main() {\n    println!(\"Hello\");\n}\n```";
    let nodes = MarkdownRenderer::render(input);
    assert!(!nodes.is_empty());
    // Should contain a CodeBlock node
    assert!(
        nodes
            .iter()
            .any(|n| matches!(n, DocumentNode::CodeBlock { .. }))
    );
}

#[test]
fn test_link() {
    let input = "See [this link](https://example.com) for more.";
    let nodes = MarkdownRenderer::render(input);
    assert!(!nodes.is_empty());
    // Should contain a Paragraph with a Span that has an action (link)
    let has_link_span = nodes.iter().any(|n| {
        if let DocumentNode::Paragraph { spans } = n {
            spans.iter().any(|s| s.action.is_some())
        } else {
            false
        }
    });
    assert!(
        has_link_span,
        "Should contain a paragraph with a span containing a link action"
    );
}

#[test]
fn test_heading() {
    let input = "# Main Title\n\n## Subsection";
    let nodes = MarkdownRenderer::render(input);
    assert!(!nodes.is_empty());
    // Should contain Heading nodes
    let headings: Vec<_> = nodes
        .iter()
        .filter(|n| matches!(n, DocumentNode::Heading { .. }))
        .collect();
    assert_eq!(headings.len(), 2, "Expected 2 heading nodes");

    // Check first is Title level
    if let DocumentNode::Heading { level, .. } = &headings[0] {
        assert!(matches!(level, HeadingLevel::Title));
    }

    // Check second is Section level
    if let DocumentNode::Heading { level, .. } = &headings[1] {
        assert!(matches!(level, HeadingLevel::Section));
    }
}

#[test]
fn test_links_in_list_items() {
    let input =
        "- Item with [link](https://example.com) inline\n- Another [link](https://other.com) here";
    let nodes = MarkdownRenderer::render(input);

    // Should have exactly one list
    let lists: Vec<_> = nodes
        .iter()
        .filter(|n| matches!(n, DocumentNode::List { .. }))
        .collect();
    assert_eq!(lists.len(), 1, "Expected exactly 1 list node");

    // Check that the list has 2 items with links properly nested
    if let DocumentNode::List { items, .. } = lists[0] {
        assert_eq!(items.len(), 2, "Expected 2 list items");

        // First item should contain a paragraph with a span containing a link
        let first_has_link = items[0].content.iter().any(|n| {
            if let DocumentNode::Paragraph { spans } = n {
                spans.iter().any(|s| s.action.is_some())
            } else {
                false
            }
        });
        assert!(first_has_link, "First list item should contain a link");

        // Second item should contain a paragraph with a span containing a link
        let second_has_link = items[1].content.iter().any(|n| {
            if let DocumentNode::Paragraph { spans } = n {
                spans.iter().any(|s| s.action.is_some())
            } else {
                false
            }
        });
        assert!(second_has_link, "Second list item should contain a link");
    } else {
        panic!("Expected a List node");
    }
}
