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

impl<'a> Span<'a> {
    pub fn url(&self) -> Option<Cow<'a, str>> {
        self.action.as_ref()?.url()
    }
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
