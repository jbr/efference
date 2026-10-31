use crate::tui::document::{DocumentNode, HeadingLevel, ListItem, Span, SpanStyle, TuiAction};
use pulldown_cmark::{BrokenLink, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Stack item for building the document tree
/// We need this because Lists contain ListItems (not DocumentNodes directly)
enum StackItem<'a> {
    Node(DocumentNode<'a>),
    Item(ListItem<'a>),
}

pub struct MarkdownRenderer;

impl MarkdownRenderer {
    /// Render markdown into a list of document nodes.
    ///
    /// Vendored from ferritin's `MarkdownRenderer`; the intra-doc link resolver
    /// (which produced rustdoc `Navigate` actions) was dropped — every link
    /// becomes a plain [`TuiAction::OpenUrl`].
    pub fn render<'a>(markdown: &str) -> Vec<DocumentNode<'a>> {
        let callback = |broken_link: BrokenLink| {
            Some((
                broken_link.reference.trim_matches('`').to_string().into(),
                broken_link.reference.clone().into_static(),
            ))
        };

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        // GFM `~~strikethrough~~` — without this the `~~` markers render literally
        // (and `Tag::Strikethrough` never fires).
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_with_broken_link_callback(markdown, options, Some(&callback));

        let mut root: Vec<DocumentNode<'a>> = Vec::new();
        let mut stack: Vec<StackItem<'a>> = Vec::new();
        let mut current_spans: Vec<Span<'a>> = Vec::new();

        // Inline style state (doesn't nest structurally)
        let mut in_code_block = false;
        let mut code_block_lang: Option<String> = None;
        let mut code_block_content = String::new();
        let mut in_strong = false;
        let mut in_emphasis = false;
        let mut in_strikethrough = false;
        let mut in_heading = false;
        let mut heading_level: Option<HeadingLevel> = None;
        let mut current_link_action: Option<TuiAction<'a>> = None;

        // Table state
        let mut in_table_head = false;
        let mut table_header: Option<Vec<crate::tui::document::TableCell<'a>>> = None;
        let mut table_rows: Vec<Vec<crate::tui::document::TableCell<'a>>> = Vec::new();
        let mut current_row: Vec<crate::tui::document::TableCell<'a>> = Vec::new();

        for event in parser {
            match event {
                Event::Start(tag) => match tag {
                    Tag::CodeBlock(kind) => {
                        in_code_block = true;
                        code_block_lang = Some(match kind {
                            CodeBlockKind::Fenced(lang) => {
                                match lang.split(',').next().unwrap_or(&*lang) {
                                    "no_run" | "should_panic" | "ignore" | "compile_fail"
                                    | "edition2015" | "edition2018" | "edition2021"
                                    | "edition2024" | "rust" | "" => "rust".to_string(),
                                    other => other.to_string(),
                                }
                            }
                            CodeBlockKind::Indented => "rust".to_string(),
                        });
                        code_block_content.clear();
                    }
                    Tag::Emphasis => {
                        in_emphasis = true;
                    }
                    Tag::Strong => {
                        in_strong = true;
                    }
                    Tag::Strikethrough => {
                        in_strikethrough = true;
                    }
                    Tag::Link { dest_url, .. } => {
                        current_link_action = Some(TuiAction::OpenUrl(dest_url.to_string().into()));
                    }
                    Tag::Heading { level, .. } => {
                        in_heading = true;
                        // Map pulldown_cmark HeadingLevel to our HeadingLevel
                        heading_level = Some(match level {
                            pulldown_cmark::HeadingLevel::H1 => HeadingLevel::Title,
                            _ => HeadingLevel::Section,
                        });
                    }
                    Tag::List(start) => {
                        // Flush any accumulated spans before starting a list
                        if !current_spans.is_empty() {
                            let para = DocumentNode::Paragraph {
                                spans: std::mem::take(&mut current_spans),
                            };
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(para));
                        }
                        // `start` is `Some(n)` for an ordered list (numbered from n),
                        // `None` for a bulleted one — carried through so the renderer
                        // can number ordered items instead of bulleting them.
                        stack.push(StackItem::Node(DocumentNode::List {
                            items: vec![],
                            start,
                        }));
                    }
                    Tag::Item => {
                        stack.push(StackItem::Item(ListItem::new(vec![])));
                    }
                    Tag::BlockQuote(_) => {
                        // Flush any accumulated spans before starting a blockquote
                        if !current_spans.is_empty() {
                            let para = DocumentNode::Paragraph {
                                spans: std::mem::take(&mut current_spans),
                            };
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(para));
                        }
                        stack.push(StackItem::Node(DocumentNode::BlockQuote { nodes: vec![] }));
                    }
                    Tag::Table(_) => {
                        table_header = None;
                        table_rows.clear();
                    }
                    Tag::TableHead => {
                        in_table_head = true;
                        current_row.clear();
                    }
                    Tag::TableRow => {
                        current_row.clear();
                    }
                    // A cell's content rides `current_spans`, and a paragraph is built
                    // at its `TagEnd`; neither has start-time work, as no other tag does.
                    _ => {}
                },
                Event::End(tag_end) => match tag_end {
                    TagEnd::Paragraph => {
                        // Create a paragraph node from collected spans
                        let paragraph_spans = std::mem::take(&mut current_spans);
                        if !paragraph_spans.is_empty() {
                            let para = DocumentNode::Paragraph {
                                spans: paragraph_spans,
                            };
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(para));
                        }
                    }
                    TagEnd::Heading(_level) if in_heading => {
                        if let Some(level) = heading_level {
                            let heading = DocumentNode::Heading {
                                level,
                                spans: std::mem::take(&mut current_spans),
                            };
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(heading));
                        }
                        in_heading = false;
                        heading_level = None;
                    }
                    TagEnd::CodeBlock if in_code_block => {
                        // Strip hidden lines for Rust code
                        let code = if matches!(code_block_lang.as_deref(), Some("rust") | None) {
                            Self::strip_hidden_lines(&code_block_content)
                        } else {
                            code_block_content.clone()
                        };

                        let code_block = DocumentNode::code_block(code_block_lang.take(), code);
                        Self::push_to_parent(&mut stack, &mut root, StackItem::Node(code_block));
                        in_code_block = false;
                    }
                    TagEnd::Emphasis => {
                        in_emphasis = false;
                    }
                    TagEnd::Strong => {
                        in_strong = false;
                    }
                    TagEnd::Strikethrough => {
                        in_strikethrough = false;
                    }
                    TagEnd::Link => {
                        // Just clear the link action - spans have already been created with it
                        current_link_action = None;
                    }
                    TagEnd::BlockQuote(_) => {
                        // Flush any remaining spans as a paragraph before closing the blockquote
                        if !current_spans.is_empty() {
                            let para = DocumentNode::Paragraph {
                                spans: std::mem::take(&mut current_spans),
                            };
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(para));
                        }

                        // Pop the blockquote and push to parent
                        if let Some(StackItem::Node(blockquote)) = stack.pop() {
                            Self::push_to_parent(
                                &mut stack,
                                &mut root,
                                StackItem::Node(blockquote),
                            );
                        }
                    }
                    TagEnd::List(_) => {
                        // Pop the list and push to parent
                        if let Some(StackItem::Node(list)) = stack.pop() {
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(list));
                        }
                    }
                    TagEnd::Item => {
                        // Flush any remaining spans as a paragraph before closing the item
                        if !current_spans.is_empty() {
                            let para = DocumentNode::Paragraph {
                                spans: std::mem::take(&mut current_spans),
                            };
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Node(para));
                        }

                        // Pop the item and push to parent list
                        if let Some(StackItem::Item(item)) = stack.pop() {
                            Self::push_to_parent(&mut stack, &mut root, StackItem::Item(item));
                        }
                    }
                    TagEnd::TableCell => {
                        // Create a table cell from collected spans
                        let cell = crate::tui::document::TableCell::new(std::mem::take(
                            &mut current_spans,
                        ));
                        current_row.push(cell);
                    }
                    TagEnd::TableHead => {
                        table_header = Some(std::mem::take(&mut current_row));
                        in_table_head = false;
                    }
                    TagEnd::TableRow if !in_table_head => {
                        table_rows.push(std::mem::take(&mut current_row));
                    }
                    TagEnd::Table => {
                        let table = DocumentNode::Table {
                            header: table_header.take(),
                            rows: std::mem::take(&mut table_rows),
                        };
                        Self::push_to_parent(&mut stack, &mut root, StackItem::Node(table));
                    }
                    _ => {}
                },
                Event::Text(text) => {
                    if in_code_block {
                        code_block_content.push_str(&text);
                    } else {
                        let style = if in_strong {
                            SpanStyle::Strong
                        } else if in_emphasis {
                            SpanStyle::Emphasis
                        } else if in_strikethrough {
                            SpanStyle::Strikethrough
                        } else {
                            SpanStyle::Plain
                        };

                        let span = Span {
                            text: text.to_string().into(),
                            style,
                            action: current_link_action.clone(),
                        };
                        current_spans.push(span);
                    }
                }
                Event::Code(code) => {
                    let mut span = Span::inline_code(code.to_string());
                    span.action = current_link_action.clone();
                    current_spans.push(span);
                }
                Event::SoftBreak => {
                    let mut span = Span::plain(" ");
                    span.action = current_link_action.clone();
                    current_spans.push(span);
                }
                Event::HardBreak => {
                    let mut span = Span::plain("\n");
                    span.action = current_link_action.clone();
                    current_spans.push(span);
                }
                Event::Rule => {
                    Self::push_to_parent(
                        &mut stack,
                        &mut root,
                        StackItem::Node(DocumentNode::HorizontalRule),
                    );
                }
                _ => {}
            }
        }

        // Flush any remaining spans as a paragraph
        if !current_spans.is_empty() {
            root.push(DocumentNode::paragraph(std::mem::take(&mut current_spans)));
        }

        root
    }

    /// Push a completed StackItem to its parent container
    fn push_to_parent<'a>(
        stack: &mut Vec<StackItem<'a>>,
        root: &mut Vec<DocumentNode<'a>>,
        item: StackItem<'a>,
    ) {
        match stack.last_mut() {
            Some(StackItem::Item(list_item)) => {
                // Push DocumentNode to ListItem's content
                match item {
                    StackItem::Node(node) => list_item.content.push(node),
                    StackItem::Item(_) => {
                        panic!(
                            "Cannot nest ListItem directly in ListItem - lists should be nested via DocumentNode::List"
                        )
                    }
                }
            }
            Some(StackItem::Node(DocumentNode::List { items, .. })) => {
                // Push ListItem to List's items
                match item {
                    StackItem::Item(list_item) => items.push(list_item),
                    StackItem::Node(_) => {
                        panic!(
                            "Cannot push DocumentNode directly to List - must be wrapped in ListItem"
                        )
                    }
                }
            }
            Some(StackItem::Node(DocumentNode::BlockQuote { nodes })) => {
                // Push DocumentNode to BlockQuote's nodes
                match item {
                    StackItem::Node(node) => nodes.push(node),
                    StackItem::Item(_) => {
                        panic!(
                            "Cannot push ListItem directly to BlockQuote - lists should be nested via DocumentNode::List"
                        )
                    }
                }
            }
            None => {
                // Push to root
                match item {
                    StackItem::Node(node) => root.push(node),
                    StackItem::Item(_) => {
                        panic!("Cannot push ListItem to root - must be inside a List")
                    }
                }
            }
            _ => {
                panic!("Unexpected parent type on stack")
            }
        }
    }

    /// Strip hidden lines from Rust code examples
    /// Lines starting with `# ` (hash followed by space) are hidden from display
    /// but included in doctests for completeness
    /// Skip lines that start with "# " (hash followed by space)
    /// Also skip lines that are just "#"
    /// But keep lines like "#[derive(...)]" or "#![feature(...)]"
    fn strip_hidden_lines(code: &str) -> String {
        code.lines()
            .filter(|line| {
                let trimmed_start = line.trim_start();
                trimmed_start != "#" && !trimmed_start.starts_with("# ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod test;
