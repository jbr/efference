//! The wire message/block shapes the projection emits, and the trailing cache breakpoint (docs/log-and-projection.md).

use super::*;

/// One message in the Anthropic `messages` array.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ApiMessage {
    pub role: Role,
    pub content: Vec<ApiBlock>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A prompt-cache breakpoint marker (docs/log-and-projection.md). `ephemeral` is the 5-minute TTL.
/// Serializes as `{"type":"ephemeral"}`. It rides on whichever content block it's
/// attached to; [`place_trailing_cache_breakpoint`] puts exactly one at the wire
/// tail each request, so the (stable) prefix before it is cache-read while only
/// the small new tail is cache-written.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CacheControl {
    Ephemeral,
}

/// A content block on the wire. `#[serde(tag = "type")]` renders the Anthropic
/// discriminator (`"type":"text"` / `"tool_use"` / `"tool_result"`).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// An extended-thinking block replayed verbatim (docs/transport.md,
    /// docs/transport.md). `signature` is the opaque token the API minted and
    /// requires echoed unchanged; both fields are stored strings projected verbatim, so the
    /// block stays byte-deterministic. Thinking blocks lead the assistant turn, before text
    /// and `tool_use`.
    Thinking {
        thinking: String,
        signature: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// A safety-encrypted thinking block — opaque `data`, no text. Replayed verbatim like
    /// [`ApiBlock::Thinking`].
    RedactedThinking {
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// A top-level image block in a user message — the
    /// `ImagePlacement::FollowingUserBlock` carrier (docs/log-and-projection.md).
    Image {
        source: ImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        // Anthropic defaults `is_error` to false; omit it when false to keep the
        // wire (and thus the cache prefix) as lean as possible.
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

/// A `tool_result`'s `content`: the API accepts a bare string or an array of
/// `text`/`image` blocks. `Text` serializes as the bare string — byte-identical
/// to when the field was `String`, so a text-only result (every result before
/// images existed) leaves the pinned wire bytes unmoved; only a result that
/// actually carries an image takes the array form
/// (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<ResultBlock>),
}

impl From<String> for ToolResultContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for ToolResultContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl ToolResultContent {
    /// The text carried by this content, image blocks excluded — the byte count
    /// consumers were reading off the `String` form before images existed.
    pub fn text_len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Blocks(blocks) => blocks
                .iter()
                .map(|b| match b {
                    ResultBlock::Text { text } => text.len(),
                    ResultBlock::Image { .. } => 0,
                })
                .sum(),
        }
    }

    /// The bare string of the `Text` form; `None` for a block array.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Blocks(_) => None,
        }
    }

    /// Whether any text block contains `needle`. Image data is never searched.
    pub fn contains(&self, needle: &str) -> bool {
        match self {
            Self::Text(text) => text.contains(needle),
            Self::Blocks(blocks) => blocks.iter().any(|b| match b {
                ResultBlock::Text { text } => text.contains(needle),
                ResultBlock::Image { .. } => false,
            }),
        }
    }
}

/// Text blocks verbatim (newline-joined in the array form); an image renders as
/// a `[image <media_type>]` marker, never its bytes.
impl std::fmt::Display for ToolResultContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Blocks(blocks) => {
                for (i, block) in blocks.iter().enumerate() {
                    if i > 0 {
                        f.write_str("\n")?;
                    }
                    match block {
                        ResultBlock::Text { text } => f.write_str(text)?,
                        ResultBlock::Image {
                            source: ImageSource::Base64 { media_type, .. },
                        } => write!(f, "[image {media_type}]")?,
                    }
                }
                Ok(())
            }
        }
    }
}

/// One block inside a block-array `tool_result` content.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultBlock {
    Text { text: String },
    Image { source: ImageSource },
}

/// The `source` of a wire image block. Base64 is the only source efference
/// emits: the stored string is projected verbatim, so the block is exactly as
/// deterministic as any other stored text (docs/log-and-projection.md).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
}

/// Place the single trailing cache breakpoint (docs/log-and-projection.md): walk back
/// from the wire tail and mark the last **text** block `ephemeral`; blocks after it (a
/// trailing tool loop's `tool_use`/`tool_result` suffix) simply go unmarked, and a wire
/// with no text block at all gets no marker. Text-only because that is where a marker
/// is portable: OpenAI's explicit prompt caching attaches breakpoints only to text
/// blocks, and a marker converted onto any other block type bills the full cache write
/// on every request without ever creating a readable entry
/// (docs/log-and-projection.md). On Anthropic the unmarked suffix is re-billed at the
/// input rate until a text block reappears — a bounded cost accepted deliberately;
/// provider-conditional placement belongs to the provider-awareness layer.
///
/// Kept out of [`project`] so projection stays a pure, byte-stable function (the
/// breakpoint moves every request; the prefix it caches does not). Idempotent
/// re-placement on an identical wire is byte-identical.
pub fn place_trailing_cache_breakpoint(messages: &mut [ApiMessage]) {
    for message in messages.iter_mut().rev() {
        for block in message.content.iter_mut().rev() {
            if let ApiBlock::Text { cache_control, .. } = block {
                *cache_control = Some(CacheControl::Ephemeral);
                return;
            }
        }
    }
}

pub(super) fn is_false(b: &bool) -> bool {
    !*b
}
