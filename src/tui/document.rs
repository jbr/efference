use std::borrow::Cow;

/// Interactive action that can be attached to a span.
///
/// Vendored from ferritin's `styled_string`; the rustdoc-navigation variants
/// (`Navigate`/`NavigateToPath`, which carried a `DocRef`/docs.rs URL) were
/// stripped — efference has no item graph to navigate. What's left is the
/// domain-agnostic core: expand a collapsed block, open a URL, pick a theme.
#[derive(Debug, Clone)]
pub enum TuiAction<'a> {
    /// Expand a truncated block (identified by index path into document tree)
    ExpandBlock(NodePath),
    /// Open an external URL in browser
    OpenUrl(Cow<'a, str>),
    /// Select a theme (interactive mode only)
    SelectTheme(Cow<'a, str>),
}

impl<'a> TuiAction<'a> {
    /// Get the URL for this action, if it has one.
    pub fn url(&self) -> Option<Cow<'a, str>> {
        match self {
            TuiAction::ExpandBlock(_) => None,
            TuiAction::OpenUrl(cow) => Some(cow.clone()),
            TuiAction::SelectTheme(_) => None,
        }
    }
}

/// Path to a node in the document tree using indices
/// Example: [2, 3, 1] means nodes[2].children[3].children[1]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodePath {
    indices: [u16; 8], // 8 levels deep should be enough
    len: u8,
}

impl NodePath {
    pub fn new() -> Self {
        Self {
            indices: [0; 8],
            len: 0,
        }
    }

    pub fn push(&mut self, index: usize) {
        if (self.len as usize) < self.indices.len() {
            self.indices[self.len as usize] = index as u16;
            self.len += 1;
        }
    }

    pub fn indices(&self) -> &[u16] {
        &self.indices[..self.len as usize]
    }
}

/// A semantic content tree for Rust documentation
#[derive(Debug, Clone)]
pub struct Document<'a> {
    pub nodes: Vec<DocumentNode<'a>>,
}

/// Condition for when to show content (used by Conditional node)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowWhen {
    /// Always show (default)
    Always,
    /// Only in interactive mode
    Interactive,
    /// Only in non-interactive mode
    NonInteractive,
}

/// A single labeled field within a [`DocumentNode::Metadata`] block.
///
/// Holds styled spans for the value so renderers can preserve navigation
/// actions on path segments (e.g. "Defined at: tokio::sync::mpsc" links).
#[derive(Debug, Clone)]
pub struct MetadataField<'a> {
    pub label: Cow<'a, str>,
    pub value: Vec<Span<'a>>,
}

impl<'a> MetadataField<'a> {
    pub fn new(label: impl Into<Cow<'a, str>>, value: Vec<Span<'a>>) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }
}

/// A node in the documentation tree
#[derive(Debug, Clone)]
pub enum DocumentNode<'a> {
    /// Block-level paragraph
    Paragraph { spans: Vec<Span<'a>> },

    /// Structured key-value metadata block (item kind, visibility, path,
    /// crate, etc). Renderers choose layout: verbose labeled lines for
    /// human-facing modes, compact single-line summary for the AI mode.
    Metadata { fields: Vec<MetadataField<'a>> },

    /// Block-level heading
    Heading {
        level: HeadingLevel,
        spans: Vec<Span<'a>>,
    },

    /// Structural section with optional title
    Section {
        title: Option<Vec<Span<'a>>>,
        nodes: Vec<DocumentNode<'a>>,
    },

    /// List of items. `start` is `None` for a bulleted list, or `Some(n)` for an
    /// ordered list numbered from `n` (so `1.`, `2.`, … render instead of bullets).
    List {
        items: Vec<ListItem<'a>>,
        start: Option<u64>,
    },

    /// Code block with syntax highlighting (from markdown fenced blocks)
    CodeBlock {
        lang: Option<Cow<'a, str>>,
        code: Cow<'a, str>,
    },

    /// Generated code with pre-styled spans (for signatures, etc.)
    GeneratedCode { spans: Vec<Span<'a>> },

    /// A unified-style diff: each line is classified (added / removed / context /
    /// hunk) so the renderer can color it (green / red / muted). Produced by the
    /// transcript projection from an edit tool's `===DIFF===` output. A leaf node
    /// (lines aren't sub-`DocumentNode`s), so it clips as one unit in the accordion.
    Diff { lines: Vec<DiffLine> },

    /// Horizontal rule/divider
    HorizontalRule,

    /// Block quote
    BlockQuote { nodes: Vec<DocumentNode<'a>> },

    /// Table
    Table {
        header: Option<Vec<TableCell<'a>>>,
        rows: Vec<Vec<TableCell<'a>>>,
    },

    /// A collapsible block: an optional accordion `header` line (with a disclosure
    /// triangle the renderer draws) over a body of `nodes`. `level` is the live
    /// expand/collapse state — `Brief` shows the first few lines with the triangle
    /// collapsed (`▶`), `Full` shows everything expanded (`▼`). In the transcript
    /// this wraps each tool call and tool result; `ExpandBlock` flips the level.
    TruncatedBlock {
        header: Option<Vec<Span<'a>>>,
        /// The leading glyph shown when the block is *not* collapsible (e.g. an
        /// exchange's `❯`/`✗` prompt). When collapsible, the renderer shows a `▶`/`▼`
        /// disclosure triangle instead. `None` falls back to a neutral `▌`.
        resting: Option<Span<'a>>,
        nodes: Vec<DocumentNode<'a>>,
        level: TruncationLevel,
    },

    /// Conditionally shown content based on render context
    /// Formatters can use this to include content that only appears in certain modes
    Conditional {
        show_when: ShowWhen,
        nodes: Vec<DocumentNode<'a>>,
    },

    /// A conversational turn — a contiguous unit authored by the user or the
    /// assistant. The renderer marks authorship *structurally* (a colored left
    /// gutter for the user; the assistant is the unmarked default voice) rather
    /// than with a per-block role label. This is the transcript's top-level unit;
    /// it replaces the old `▌ you` / `▌ assistant` markers.
    Turn {
        author: Author,
        blocks: Vec<DocumentNode<'a>>,
    },
}

/// Who authored a [`DocumentNode::Turn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Author {
    User,
    Assistant,
}

/// One classified line of a [`DocumentNode::Diff`]. `text` keeps the original
/// leading ` `/`+`/`-` so columns align; `kind` drives the color.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// What a [`DiffLine`] represents — the renderer maps this to a color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Added,
    Removed,
    Context,
    Hunk,
}

/// A single cell in a table
#[derive(Debug, Clone)]
pub struct TableCell<'a> {
    pub spans: Vec<Span<'a>>,
}

/// A single item in a list
#[derive(Debug, Clone)]
pub struct ListItem<'a> {
    pub content: Vec<DocumentNode<'a>>,
}

/// Heading level for semantic structure
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingLevel {
    Title,   // Top-level item name: "Item: Vec"
    Section, // Section header: "Fields:", "Methods:"
}

/// Truncation level hint for renderers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationLevel {
    /// Single-line summary (for listings)
    SingleLine,
    /// Brief paragraph (for secondary items like methods/fields)
    Brief,
    /// Full documentation (for main requested item)
    Full,
}

/// A styled text span with semantic meaning
#[derive(Debug, Clone)]
pub struct Span<'a> {
    pub text: Cow<'a, str>,
    pub style: SpanStyle,
    pub action: Option<TuiAction<'a>>,
}

impl<'a> Span<'a> {
    pub fn url(&self) -> Option<Cow<'a, str>> {
        self.action.as_ref()?.url()
    }
}

/// Semantic styling categories for Rust code elements
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpanStyle {
    // Rust code semantic elements
    Keyword,      // struct, enum, pub, fn, const, etc.
    TypeName,     // MyStruct, Vec, String, etc.
    FunctionName, // my_function, new, etc.
    FieldName,    // field names in structs
    Lifetime,     // 'a, 'static, etc.
    Generic,      // T, U, generic parameters

    // Structural elements
    Plain,       // unstyled text, whitespace
    Punctuation, // :, {, }, (, ), etc.
    Operator,    // &, *, ->, etc.
    Comment,     // // comments in code output

    // Code content
    InlineRustCode, // Inline code expressions (const values, etc.) - unparsed Rust code
    InlineCode,     // Generic inline code from markdown backticks

    // Markdown semantic styles
    Strong,        // **bold** - semantic emphasis
    Emphasis,      // *italic* - semantic emphasis
    Strikethrough, // ~~strikethrough~~ - from GFM

    // Diff lines
    Added,   // +added line (green)
    Removed, // -removed line (red)

    // Diagnostics
    Error, // a failed tool result / error label (red) — distinct from Strikethrough,
           // which means deletion/redaction, not failure

    // Conversation
    UserAccent, // the user turn's gutter / accent — distinguishes the human's voice
}

impl<'a> Span<'a> {
    // Rust code element constructors
    pub fn keyword(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Keyword,
            action: None,
        }
    }

    pub fn type_name(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::TypeName,
            action: None,
        }
    }

    pub fn function_name(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::FunctionName,
            action: None,
        }
    }

    pub fn field_name(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::FieldName,
            action: None,
        }
    }

    pub fn lifetime(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Lifetime,
            action: None,
        }
    }

    pub fn generic(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Generic,
            action: None,
        }
    }

    // Structural element constructors
    pub fn plain(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Plain,
            action: None,
        }
    }

    pub fn punctuation(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Punctuation,
            action: None,
        }
    }

    pub fn operator(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Operator,
            action: None,
        }
    }

    pub fn comment(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Comment,
            action: None,
        }
    }

    pub fn inline_rust_code(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::InlineRustCode,
            action: None,
        }
    }

    pub fn inline_code(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::InlineCode,
            action: None,
        }
    }

    pub fn strong(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Strong,
            action: None,
        }
    }

    pub fn emphasis(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Emphasis,
            action: None,
        }
    }

    pub fn strikethrough(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Strikethrough,
            action: None,
        }
    }

    /// Chainable method to attach an action to this span
    pub fn with_action(mut self, action: TuiAction<'a>) -> Self {
        self.action = Some(action);
        self
    }
}

impl<'a> Document<'a> {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn with_nodes(nodes: Vec<DocumentNode<'a>>) -> Self {
        Self { nodes }
    }
}

impl<'a> Default for Document<'a> {
    fn default() -> Self {
        Self::new()
    }
}

// Into<Document> conversions for ergonomic render() calls
impl<'a> From<Vec<DocumentNode<'a>>> for Document<'a> {
    fn from(nodes: Vec<DocumentNode<'a>>) -> Self {
        Self { nodes }
    }
}

impl<'a, 'b> From<&'b Document<'a>> for Document<'a> {
    fn from(doc: &'b Document<'a>) -> Self {
        doc.clone()
    }
}

impl<'a> ListItem<'a> {
    pub fn new(content: Vec<DocumentNode<'a>>) -> Self {
        Self { content }
    }
}

impl<'a> DocumentNode<'a> {
    /// Convenience constructor for a paragraph
    pub fn paragraph(spans: Vec<Span<'a>>) -> Self {
        DocumentNode::Paragraph { spans }
    }

    /// Convenience constructor for a metadata block
    pub fn metadata(fields: Vec<MetadataField<'a>>) -> Self {
        DocumentNode::Metadata { fields }
    }

    /// Convenience constructor for a heading
    pub fn heading(level: HeadingLevel, spans: Vec<Span<'a>>) -> Self {
        DocumentNode::Heading { level, spans }
    }

    /// Convenience constructor for a section with title
    pub fn section(title: Vec<Span<'a>>, nodes: Vec<DocumentNode<'a>>) -> Self {
        DocumentNode::Section {
            title: Some(title),
            nodes,
        }
    }

    /// Convenience constructor for a section without title
    pub fn section_untitled(nodes: Vec<DocumentNode<'a>>) -> Self {
        DocumentNode::Section { title: None, nodes }
    }

    /// Convenience constructor for a bulleted list.
    pub fn list(items: Vec<ListItem<'a>>) -> Self {
        DocumentNode::List { items, start: None }
    }

    /// Convenience constructor for a code block
    pub fn code_block(
        lang: Option<impl Into<Cow<'a, str>>>,
        code: impl Into<Cow<'a, str>>,
    ) -> Self {
        DocumentNode::CodeBlock {
            lang: lang.map(Into::into),
            code: code.into(),
        }
    }

    /// Convenience constructor for generated code
    pub fn generated_code(spans: Vec<Span<'a>>) -> Self {
        DocumentNode::GeneratedCode { spans }
    }

    /// Convenience constructor for a horizontal rule
    pub fn horizontal_rule() -> Self {
        DocumentNode::HorizontalRule
    }

    /// Convenience constructor for a block quote
    pub fn block_quote(nodes: Vec<DocumentNode<'a>>) -> Self {
        DocumentNode::BlockQuote { nodes }
    }

    /// Convenience constructor for a table
    pub fn table(header: Option<Vec<TableCell<'a>>>, rows: Vec<Vec<TableCell<'a>>>) -> Self {
        DocumentNode::Table { header, rows }
    }

    /// Convenience constructor for a header-less truncated block.
    pub fn truncated_block(nodes: Vec<DocumentNode<'a>>, level: TruncationLevel) -> Self {
        DocumentNode::TruncatedBlock { header: None, resting: None, nodes, level }
    }

    /// Convenience constructor for a diff node.
    pub fn diff(lines: Vec<DiffLine>) -> Self {
        DocumentNode::Diff { lines }
    }

    /// Convenience constructor for a collapsible block with an accordion `header`
    /// line and a `resting` leading glyph (shown when not collapsible; replaced by a
    /// disclosure triangle when there's more than fits).
    pub fn collapsible(
        header: Vec<Span<'a>>,
        resting: Option<Span<'a>>,
        nodes: Vec<DocumentNode<'a>>,
        level: TruncationLevel,
    ) -> Self {
        DocumentNode::TruncatedBlock { header: Some(header), resting, nodes, level }
    }
}

impl<'a> TableCell<'a> {
    /// Create a new table cell from spans
    pub fn new(spans: Vec<Span<'a>>) -> Self {
        Self { spans }
    }

    /// Create a table cell from a single span
    pub fn from_span(span: Span<'a>) -> Self {
        Self { spans: vec![span] }
    }
}

// Compile-time thread-safety assertions for Document
//
// Document<'a> contains DocumentNode<'a> which may hold DocRef<'a> references.
// For the threading model to work, Document must be Send so it can be passed
// between threads in scoped thread scenarios (formatting thread → UI thread).
#[allow(dead_code)]
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}

    // Document<'a> must be Send (can cross thread boundaries in scoped threads)
    const fn check_document_send() {
        assert_send::<Document<'_>>();
    }

    // Document<'a> should also be Sync (multiple threads can hold &Document safely)
    const fn check_document_sync() {
        assert_sync::<Document<'_>>();
    }
};

#[cfg(test)]
mod tests {
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
}
