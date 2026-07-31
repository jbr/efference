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
    ToolResult {
        tool_use_id: String,
        content: String,
        // Anthropic defaults `is_error` to false; omit it when false to keep the
        // wire (and thus the cache prefix) as lean as possible.
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
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
