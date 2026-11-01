use crate::clients::tui::document::{
    DocumentNode, HeadingLevel, ListItem, Span, SpanStyle, TableCell, TuiAction,
};
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

        let mut tree = Tree::default();
        for event in parser {
            tree.event(event);
        }
        tree.finish()
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

/// Which inline styles are open.
#[derive(Default)]
struct InlineStyle {
    strong: bool,
    emphasis: bool,
    strikethrough: bool,
}

/// The document tree under construction as the parser's events arrive.
#[derive(Default)]
struct Tree<'a> {
    root: Vec<DocumentNode<'a>>,
    stack: Vec<StackItem<'a>>,
    current_spans: Vec<Span<'a>>,

    // Inline style state (doesn't nest structurally)
    in_code_block: bool,
    code_block_lang: Option<String>,
    code_block_content: String,
    inline: InlineStyle,
    in_heading: bool,
    heading_level: Option<HeadingLevel>,
    current_link_action: Option<TuiAction<'a>>,

    // Table state
    in_table_head: bool,
    table_header: Option<Vec<TableCell<'a>>>,
    table_rows: Vec<Vec<TableCell<'a>>>,
    current_row: Vec<TableCell<'a>>,
}

impl<'a> Tree<'a> {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag_end) => self.end(tag_end),
            Event::Text(text) => {
                if self.in_code_block {
                    self.code_block_content.push_str(&text);
                } else {
                    let style = if self.inline.strong {
                        SpanStyle::Strong
                    } else if self.inline.emphasis {
                        SpanStyle::Emphasis
                    } else if self.inline.strikethrough {
                        SpanStyle::Strikethrough
                    } else {
                        SpanStyle::Plain
                    };

                    let span = Span {
                        text: text.to_string().into(),
                        style,
                        action: self.current_link_action.clone(),
                    };
                    self.current_spans.push(span);
                }
            }
            Event::Code(code) => {
                let mut span = Span::inline_code(code.to_string());
                span.action = self.current_link_action.clone();
                self.current_spans.push(span);
            }
            Event::SoftBreak => {
                let mut span = Span::plain(" ");
                span.action = self.current_link_action.clone();
                self.current_spans.push(span);
            }
            Event::HardBreak => {
                let mut span = Span::plain("\n");
                span.action = self.current_link_action.clone();
                self.current_spans.push(span);
            }
            Event::Rule => {
                self.push_to_parent(StackItem::Node(DocumentNode::HorizontalRule));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_block_lang = Some(match kind {
                    CodeBlockKind::Fenced(lang) => match lang.split(',').next().unwrap_or(&*lang) {
                        "no_run" | "should_panic" | "ignore" | "compile_fail" | "edition2015"
                        | "edition2018" | "edition2021" | "edition2024" | "rust" | "" => {
                            "rust".to_string()
                        }
                        other => other.to_string(),
                    },
                    CodeBlockKind::Indented => "rust".to_string(),
                });
                self.code_block_content.clear();
            }
            Tag::Emphasis => {
                self.inline.emphasis = true;
            }
            Tag::Strong => {
                self.inline.strong = true;
            }
            Tag::Strikethrough => {
                self.inline.strikethrough = true;
            }
            Tag::Link { dest_url, .. } => {
                self.current_link_action = Some(TuiAction::OpenUrl(dest_url.to_string().into()));
            }
            Tag::Heading { level, .. } => {
                self.in_heading = true;
                // Map pulldown_cmark HeadingLevel to our HeadingLevel
                self.heading_level = Some(match level {
                    pulldown_cmark::HeadingLevel::H1 => HeadingLevel::Title,
                    _ => HeadingLevel::Section,
                });
            }
            Tag::List(start) => {
                // Flush any accumulated spans before starting a list
                self.flush_paragraph();
                // `start` is `Some(n)` for an ordered list (numbered from n),
                // `None` for a bulleted one — carried through so the renderer
                // can number ordered items instead of bulleting them.
                self.stack.push(StackItem::Node(DocumentNode::List {
                    items: vec![],
                    start,
                }));
            }
            Tag::Item => {
                self.stack.push(StackItem::Item(ListItem::new(vec![])));
            }
            Tag::BlockQuote(_) => {
                // Flush any accumulated spans before starting a blockquote
                self.flush_paragraph();
                self.stack
                    .push(StackItem::Node(DocumentNode::BlockQuote { nodes: vec![] }));
            }
            Tag::Table(_) => {
                self.table_header = None;
                self.table_rows.clear();
            }
            Tag::TableHead => {
                self.in_table_head = true;
                self.current_row.clear();
            }
            Tag::TableRow => {
                self.current_row.clear();
            }
            // A cell's content rides `current_spans`, and a paragraph is built
            // at its `TagEnd`; neither has start-time work, as no other tag does.
            _ => {}
        }
    }

    fn end(&mut self, tag_end: TagEnd) {
        match tag_end {
            TagEnd::Paragraph => {
                // Create a paragraph node from collected spans
                let paragraph_spans = std::mem::take(&mut self.current_spans);
                if !paragraph_spans.is_empty() {
                    let para = DocumentNode::Paragraph {
                        spans: paragraph_spans,
                    };
                    self.push_to_parent(StackItem::Node(para));
                }
            }
            TagEnd::Heading(_level) if self.in_heading => {
                if let Some(level) = self.heading_level {
                    let heading = DocumentNode::Heading {
                        level,
                        spans: std::mem::take(&mut self.current_spans),
                    };
                    self.push_to_parent(StackItem::Node(heading));
                }
                self.in_heading = false;
                self.heading_level = None;
            }
            TagEnd::CodeBlock if self.in_code_block => {
                // Strip hidden lines for Rust code
                let code = if matches!(self.code_block_lang.as_deref(), Some("rust") | None) {
                    MarkdownRenderer::strip_hidden_lines(&self.code_block_content)
                } else {
                    self.code_block_content.clone()
                };

                let code_block = DocumentNode::code_block(self.code_block_lang.take(), code);
                self.push_to_parent(StackItem::Node(code_block));
                self.in_code_block = false;
            }
            TagEnd::Emphasis => {
                self.inline.emphasis = false;
            }
            TagEnd::Strong => {
                self.inline.strong = false;
            }
            TagEnd::Strikethrough => {
                self.inline.strikethrough = false;
            }
            TagEnd::Link => {
                // Just clear the link action - spans have already been created with it
                self.current_link_action = None;
            }
            TagEnd::BlockQuote(_) => {
                // Flush any remaining spans as a paragraph before closing the blockquote
                self.flush_paragraph();

                // Pop the blockquote and push to parent
                if let Some(StackItem::Node(blockquote)) = self.stack.pop() {
                    self.push_to_parent(StackItem::Node(blockquote));
                }
            }
            TagEnd::List(_) => {
                // Pop the list and push to parent
                if let Some(StackItem::Node(list)) = self.stack.pop() {
                    self.push_to_parent(StackItem::Node(list));
                }
            }
            TagEnd::Item => {
                // Flush any remaining spans as a paragraph before closing the item
                self.flush_paragraph();

                // Pop the item and push to parent list
                if let Some(StackItem::Item(item)) = self.stack.pop() {
                    self.push_to_parent(StackItem::Item(item));
                }
            }
            TagEnd::TableCell => {
                // Create a table cell from collected spans
                let cell = TableCell::new(std::mem::take(&mut self.current_spans));
                self.current_row.push(cell);
            }
            TagEnd::TableHead => {
                self.table_header = Some(std::mem::take(&mut self.current_row));
                self.in_table_head = false;
            }
            TagEnd::TableRow if !self.in_table_head => {
                self.table_rows.push(std::mem::take(&mut self.current_row));
            }
            TagEnd::Table => {
                let table = DocumentNode::Table {
                    header: self.table_header.take(),
                    rows: std::mem::take(&mut self.table_rows),
                };
                self.push_to_parent(StackItem::Node(table));
            }
            _ => {}
        }
    }

    /// Close the paragraph the accumulated spans make, if there are any.
    fn flush_paragraph(&mut self) {
        if !self.current_spans.is_empty() {
            let para = DocumentNode::Paragraph {
                spans: std::mem::take(&mut self.current_spans),
            };
            self.push_to_parent(StackItem::Node(para));
        }
    }

    /// The tree, with any spans still accumulated closed as a last paragraph.
    fn finish(mut self) -> Vec<DocumentNode<'a>> {
        // Flush any remaining spans as a paragraph
        if !self.current_spans.is_empty() {
            self.root.push(DocumentNode::paragraph(std::mem::take(
                &mut self.current_spans,
            )));
        }

        self.root
    }

    /// Push a completed StackItem to its parent container
    fn push_to_parent(&mut self, item: StackItem<'a>) {
        match self.stack.last_mut() {
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
                    StackItem::Node(node) => self.root.push(node),
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
}

#[cfg(test)]
mod test;
