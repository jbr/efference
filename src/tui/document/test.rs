
use super::*;

#[test]
fn test_span_creation() {
    let span = Span::keyword("struct");
    assert_eq!(span.text, "struct");
    assert!(matches!(span.style, SpanStyle::Keyword));
}

#[test]
fn test_section() {
    let section = DocumentNode::section(
        vec![Span::plain("Fields:")],
        vec![DocumentNode::list(vec![
            ListItem::new(vec![DocumentNode::paragraph(vec![Span::field_name("x")])]),
            ListItem::new(vec![DocumentNode::paragraph(vec![Span::field_name("y")])]),
        ])],
    );

    if let DocumentNode::Section { title, nodes } = section {
        assert!(title.is_some());
        assert_eq!(nodes.len(), 1);
    } else {
        panic!("Expected section node");
    }
}

#[test]
fn test_list_items() {
    let list = DocumentNode::list(vec![
        ListItem::new(vec![DocumentNode::paragraph(vec![
            Span::field_name("foo"),
            Span::punctuation(":"),
            Span::type_name("u32"),
        ])]),
        ListItem::new(vec![DocumentNode::paragraph(vec![Span::field_name("bar")])]),
    ]);

    if let DocumentNode::List { items, .. } = list {
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].content.len(), 1);
        assert_eq!(items[1].content.len(), 1);
    } else {
        panic!("Expected list node");
    }
}

#[test]
fn test_heading_levels() {
    let title = DocumentNode::heading(HeadingLevel::Title, vec![Span::plain("Item: Vec")]);
    let section = DocumentNode::heading(HeadingLevel::Section, vec![Span::plain("Methods:")]);

    assert!(matches!(title, DocumentNode::Heading { .. }));
    assert!(matches!(section, DocumentNode::Heading { .. }));
}

#[test]
fn test_code_block() {
    let code = DocumentNode::code_block(Some("rust".to_string()), "fn main() {}".to_string());

    if let DocumentNode::CodeBlock { lang, code } = code {
        assert_eq!(lang, Some("rust".into()));
        assert_eq!(code, "fn main() {}");
    } else {
        panic!("Expected code block");
    }
}
