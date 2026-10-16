use std::borrow::Cow;

/// Interactive action that can be attached to a span.
///
/// Vendored from ferritin's `styled_string`; the rustdoc-navigation variants
/// (`Navigate`/`NavigateToPath`, which carried a `DocRef`/docs.rs URL) were
/// stripped — efference has no item graph to navigate. What's left is the
/// domain-agnostic core: expand a collapsed block, open a URL, pick a theme.
#[derive(Debug, Clone)]
pub enum TuiAction<'a> {
    /// A transcript [`Entry`](DocumentNode::Entry)'s focus target (identified by
    /// index path into the document tree). Focus is all it does: entries render
    /// whole, so there is nothing here to expand (docs/tui.md).
    FocusEntry(NodePath),
    /// Open an external URL in browser
    OpenUrl(Cow<'a, str>),
    /// Select a theme (interactive mode only)
    SelectTheme(Cow<'a, str>),
    /// Open a folded transcript item whole (docs/tui.md, "Detail by recency"), by
    /// the item's key — a call's `call_id`, a thinking block's position.
    ExpandItem(Cow<'a, str>),
}

impl<'a> TuiAction<'a> {
    /// Get the URL for this action, if it has one.
    pub fn url(&self) -> Option<Cow<'a, str>> {
        match self {
            TuiAction::FocusEntry(_) => None,
            TuiAction::OpenUrl(cow) => Some(cow.clone()),
            TuiAction::SelectTheme(_) | TuiAction::ExpandItem(_) => None,
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

    /// A run of consecutive lines with **no blank row between them** — one row per
    /// line, each wrapping like a paragraph.
    ///
    /// The distinction from a sequence of [`Paragraph`](Self::Paragraph)s is the
    /// spacing: an [`Entry`](Self::Entry) separates its children by a blank row,
    /// which is markdown block spacing and is right for prose. Text that is already
    /// line-structured — a running tool's output, a thinking phase, an arriving tool
    /// input, a multi-line user turn — is not a sequence of blocks, and rendering it
    /// as one doubles every newline the source actually had.
    Lines { lines: Vec<Vec<Span<'a>>> },

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

    /// Render the children in the **dimmed register**: present, but visibly not
    /// the assistant's answer. Thinking is the case it exists for — it is
    /// markdown like prose is, so it goes through the same renderer, but it must
    /// never be mistaken for the answer, and the TUI has no cascade to inherit a
    /// muted color from the way a CSS class does (docs/tui.md).
    ///
    /// Wrapping rather than a dim variant per style: the register is a property
    /// of *where* a node sits, not of what the span means, so the markdown
    /// renderer keeps emitting the same semantic styles either way.
    Dimmed { nodes: Vec<DocumentNode<'a>> },

    /// Code block with syntax highlighting (from markdown fenced blocks)
    CodeBlock {
        lang: Option<Cow<'a, str>>,
        code: Cow<'a, str>,
    },

    /// Generated code with pre-styled spans (for signatures, etc.)
    GeneratedCode { spans: Vec<Span<'a>> },

    /// A code block whose lines carry their **original line numbers**, in a dimmed
    /// gutter. Gaps between non-contiguous runs are implied by the jump in the
    /// numbers, the way a `@@` header would have stated them (docs/tui.md).
    ///
    /// This is the TUI's form for a triaged tool result. The wire has to say
    /// "these lines came from there" in text, so it uses `@@ lines a-b @@` headers
    /// (docs/triage.md); a terminal has a gutter, and can say the same thing
    /// without spending a row on it.
    NumberedCode {
        lang: Option<Cow<'a, str>>,
        /// `(original line number, text)`, ascending.
        lines: Vec<(u32, Cow<'a, str>)>,
    },

    /// A unified-style diff: each line is classified (added / removed / context /
    /// hunk) so the renderer can color it (green / red / muted). Produced by the
    /// transcript projection from an edit tool's `===DIFF===` output. A leaf node
    /// (lines aren't sub-`DocumentNode`s), so it clips as one unit in the accordion.
    Diff { lines: Vec<DiffLine> },

    /// One logged result image (docs/tui.md), by reference: the log address the
    /// bytes resolve through (`GET /log/:seq/images/:idx`) plus the pixel
    /// dimensions from the entry view's metadata. The node carries **no pixel
    /// data and no encoding** — those live in the state's image store, keyed by
    /// `(seq, idx)` — so the projection stays pure of the terminal's graphics
    /// capability, and the reserved footprint (a pure function of these
    /// dimensions and the render width) never changes as bytes arrive, encode,
    /// or fail.
    Image {
        seq: u64,
        idx: usize,
        media_type: Cow<'a, str>,
        /// Pixel dimensions of the encoded image, from the log metadata.
        width: u32,
        height: u32,
    },

    /// Horizontal rule/divider
    HorizontalRule,

    /// Block quote
    BlockQuote { nodes: Vec<DocumentNode<'a>> },

    /// Table
    Table {
        header: Option<Vec<TableCell<'a>>>,
        rows: Vec<Vec<TableCell<'a>>>,
    },

    /// Conditionally shown content based on render context
    /// Formatters can use this to include content that only appears in certain modes
    Conditional {
        show_when: ShowWhen,
        nodes: Vec<DocumentNode<'a>>,
    },

    /// One tool call as a transcript item (docs/tui.md, "Detail by recency"): the
    /// call entry and everything that reads as part of it — what the spotter made of
    /// it, the triage affordance, the result, the triage note — kept together so the
    /// renderer can fold the whole item by its distance from the tail. Folding is a
    /// render-time choice; the children are exactly the entries a whole call renders.
    CallItem {
        /// The call's `call_id`: what expansion is keyed by.
        key: Cow<'a, str>,
        /// Renders whole at any distance: still running or held at a gate, failed,
        /// or flagged by the spotter.
        must_show: bool,
        /// What the brief form is made of: the intent, the signature on one line,
        /// and the triage note.
        intent: Option<String>,
        signature: String,
        note: Option<String>,
        nodes: Vec<DocumentNode<'a>>,
    },

    /// A committed thinking entry as a transcript item (docs/tui.md, "Detail by
    /// recency"): a long one folds by its distance from the tail. `words` decides
    /// whether it may fold at all; `entry` is exactly what a whole block renders.
    ThinkingItem {
        /// The assistant entry's seq: what expansion is keyed by.
        key: Cow<'a, str>,
        words: usize,
        entry: Box<DocumentNode<'a>>,
    },

    /// A block with a caption line directly above it and no blank row between —
    /// a call argument's key over its content (docs/tui.md), the way the web
    /// client's `<dt>` sits on its `<dd>`.
    Captioned {
        caption: Vec<Span<'a>>,
        nodes: Vec<DocumentNode<'a>>,
    },

    /// A call's signature (docs/tui.md, after docs/web-client.md "A call reads as a
    /// signature"): laid out at render time, where the width is known, so it breaks
    /// across rows the way the web client's does. Arguments too long to sit in it
    /// at that width render beneath it as labelled text. `arriving` marks a call
    /// whose input is still streaming: the `…` goes inside the parentheses, so the
    /// signature never reads as closed before it is.
    Signature {
        view: Box<crate::tui::projection::signature::CallView>,
        arriving: bool,
    },

    /// One transcript entry: the top-level unit of the TUI's transcript, and its
    /// unit of focus (docs/tui.md). The transcript is a **flat** sequence of these,
    /// one per top-level thing on the wire — a message, a thinking block, a tool
    /// call, a tool result — decorated for presentation rather than grouped into
    /// conversational turns.
    ///
    /// An entry renders whole; folding is the item's business, decided at render
    /// time by its distance from the tail (`CallItem`, `ThinkingItem`; docs/tui.md,
    /// "Detail by recency").
    Entry {
        /// What this entry *is* — which drives its whole presentation (marker,
        /// gutter, register) and which verbs it carries.
        kind: EntryKind,
        /// An optional leading glyph on the header row (`❯` for a call, `✓`/`✗` for
        /// a result). Rendered only when there is a `header`.
        marker: Option<Span<'a>>,
        /// The entry's own first line, above its body. `None` for entries that are
        /// nothing but their content (a message, an annotation).
        header: Option<Vec<Span<'a>>>,
        blocks: Vec<DocumentNode<'a>>,
        /// A stable identity that survives re-projection (docs/tui.md). The
        /// transcript re-projects on every streamed token, rebuilding the tree, so
        /// the *positional* [`NodePath`] is ephemeral — `key` lets `tui::state`
        /// carry per-entry view-state (raw-vs-triaged) across rebuilds, keyed by
        /// the result's `call_id`. `None` for entries with nothing to carry.
        key: Option<Cow<'a, str>>,
    },
}

/// What a transcript [`Entry`](DocumentNode::Entry) is — the three things an
/// operator scans for, split so each can be styled to be told apart at a glance:
/// **what I said** ([`UserMessage`](EntryKind::UserMessage)), **what the agent
/// said** ([`AssistantMessage`](EntryKind::AssistantMessage),
/// [`Thinking`](EntryKind::Thinking)), and **what the agent did**
/// ([`ToolCall`](EntryKind::ToolCall), [`ToolResult`](EntryKind::ToolResult)).
///
/// A tool result occupies a `role: user` block on the wire, but it is the harness
/// speaking rather than the operator, so it is its own kind and never carries the
/// operator's gutter. The wire's roles are not the taxonomy the reader wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// What the operator said — carries the accent gutter.
    UserMessage,
    /// What the agent said, as its committed prose.
    AssistantMessage,
    /// What the agent thought, in the dimmed register (docs/tui.md).
    Thinking,
    /// A tool call the agent made.
    ToolCall,
    /// What a call returned.
    ToolResult,
    /// What a call is returning — a still-running tool's output, which is
    /// unbounded and untriaged and so is the one entry the transcript may show a
    /// peek of (docs/tui.md). Distinct from [`ToolResult`](Self::ToolResult)
    /// because the two carry different verbs: a committed result forks
    /// triaged-vs-raw, a running one forks peek-vs-full.
    RunningTool,
    /// Something the *harness* says about a call — a spotter verdict, the approval
    /// affordance. Not agent-generated, so it is never a focus stop and never a
    /// child of the call it annotates (docs/tui.md).
    Annotation,
}

impl EntryKind {
    /// Whether `↑`/`↓` stops here. Everything the agent or operator produced is a
    /// focus unit; the harness's own annotations are not — they are read beside the
    /// call, not visited.
    pub fn is_focusable(self) -> bool {
        !matches!(self, EntryKind::Annotation)
    }

    /// Whether this entry opens a new visual group, i.e. gets a blank line above it.
    /// A result and an annotation are continuations of the call they belong to, so
    /// the call, what the harness made of it, and what came back read as one unit.
    pub fn starts_group(self) -> bool {
        !matches!(
            self,
            EntryKind::ToolResult | EntryKind::RunningTool | EntryKind::Annotation
        )
    }
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

    /// Convenience constructor for a run of unspaced lines. `lines` is one span
    /// list per row.
    pub fn lines(lines: Vec<Vec<Span<'a>>>) -> Self {
        DocumentNode::Lines { lines }
    }

    /// A run of unspaced lines each carrying one span — the live tail's shape.
    pub fn styled_lines(lines: impl IntoIterator<Item = Span<'a>>) -> Self {
        DocumentNode::Lines {
            lines: lines.into_iter().map(|span| vec![span]).collect(),
        }
    }

    /// Render `nodes` in the dimmed register ([`DocumentNode::Dimmed`]).
    pub fn dimmed(nodes: Vec<DocumentNode<'a>>) -> Self {
        DocumentNode::Dimmed { nodes }
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

    /// Convenience constructor for a diff node.
    pub fn diff(lines: Vec<DiffLine>) -> Self {
        DocumentNode::Diff { lines }
    }

    /// Convenience constructor for a result-image reference.
    pub fn image(
        seq: u64,
        idx: usize,
        media_type: impl Into<Cow<'a, str>>,
        width: u32,
        height: u32,
    ) -> Self {
        DocumentNode::Image {
            seq,
            idx,
            media_type: media_type.into(),
            width,
            height,
        }
    }

    /// Convenience constructor for a line-numbered code block.
    pub fn numbered_code(
        lang: Option<impl Into<Cow<'a, str>>>,
        lines: Vec<(u32, Cow<'a, str>)>,
    ) -> Self {
        DocumentNode::NumberedCode {
            lang: lang.map(Into::into),
            lines,
        }
    }

    /// A call's signature node (see [`DocumentNode::Signature`]).
    pub fn signature(view: crate::tui::projection::signature::CallView, arriving: bool) -> Self {
        DocumentNode::Signature {
            view: Box::new(view),
            arriving,
        }
    }

    /// A transcript entry that is nothing but its content — a message, an
    /// annotation. The kind is what the renderer decorates it by.
    pub fn entry(kind: EntryKind, blocks: Vec<DocumentNode<'a>>) -> Self {
        DocumentNode::Entry {
            kind,
            marker: None,
            header: None,
            blocks,
            key: None,
        }
    }

    /// A transcript entry led by its own header row: a marker glyph, the header
    /// spans, and the [block menu](crate::tui::block_menu) when focused.
    pub fn headed_entry(
        kind: EntryKind,
        marker: Option<Span<'a>>,
        header: Vec<Span<'a>>,
        blocks: Vec<DocumentNode<'a>>,
    ) -> Self {
        DocumentNode::Entry {
            kind,
            marker,
            header: Some(header),
            blocks,
            key: None,
        }
    }

    /// Attach a durable [`key`](DocumentNode::Entry) so `tui::state` can persist
    /// this entry's view-state across re-projection (docs/tui.md) — used for tool
    /// results, keyed by `call_id`.
    pub fn keyed(mut self, entry_key: impl Into<Cow<'a, str>>) -> Self {
        if let DocumentNode::Entry { key, .. } = &mut self {
            *key = Some(entry_key.into());
        }
        self
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
mod test;

mod span;

pub use span::{Span, SpanStyle};
