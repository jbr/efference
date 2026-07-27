//! Inline spans and their styles — the leaf of the document IR.

use super::*;

/// A styled text span with semantic meaning
#[derive(Debug, Clone)]
pub struct Span<'a> {
    pub text: Cow<'a, str>,
    pub style: SpanStyle,
    pub action: Option<TuiAction<'a>>,
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
