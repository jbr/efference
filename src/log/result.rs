//! A tool call's raw result and what it carries besides text (docs/log-and-projection.md).

use super::*;

/// The full, immutable result of a tool run. Survives in the log forever; never
/// overwritten by triage — that is what makes every triage reversible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawResult {
    pub content: String,
    pub is_error: bool,
    /// Set **only by the control-channel entry view**, and only on the window and
    /// frame copies of an oversized result: it says the `content` beside it is a
    /// prefix, and how much of the whole string that prefix is
    /// (`daemon/entry_view.rs`, docs/control-api.md). The mirror of
    /// [`ResultImage::data`], which the same views omit — a Rust client parsing a
    /// view with this shared type reads the elision as a value rather than
    /// inferring it from a length. The log itself always writes `content` whole
    /// and never writes this field, so it is `None` in every in-process result and
    /// absent from every logged line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_elided: Option<ContentElision>,
    /// Images the tool returned alongside `content` (docs/log-and-projection.md).
    /// The realized bytes live here, in the log entry itself, so projection is a
    /// copy of stored data — never a re-read or re-encode of anything on disk.
    /// Encoding happens once, at tool-run time. Defaulted and skipped when empty,
    /// so text-only results (and every existing log) serialize byte-identically
    /// to before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ResultImage>,
}

impl RawResult {
    /// A text result with no images and no elision, flagged as the caller says.
    /// [`text`](Self::text) and [`error`](Self::error) name the two common cases.
    pub fn new(content: impl Into<String>, is_error: bool) -> Self {
        Self {
            content: content.into(),
            is_error,
            content_elided: None,
            images: Vec::new(),
        }
    }

    /// A successful text result.
    pub fn text(content: impl Into<String>) -> Self {
        Self::new(content, false)
    }

    /// An error result.
    pub fn error(content: impl Into<String>) -> Self {
        Self::new(content, true)
    }

    /// Attach the images the tool returned beside `content`. Builds; a recorded
    /// result is never amended.
    pub fn with_images(mut self, images: Vec<ResultImage>) -> Self {
        self.images = images;
        self
    }
}

/// The tag on a `content` a control-channel view truncated: how many bytes the view
/// carries, and how many the whole result holds. `GET /log/:seq` serves the rest
/// (docs/control-api.md).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentElision {
    pub kept_bytes: usize,
    pub total_bytes: usize,
}

/// One image in a [`RawResult`]: the exact bytes projected onto the wire, plus
/// the dimensions of the image as encoded. `width`/`height` are log metadata
/// (audit, TUI, token math) — the wire image block carries only `media_type` +
/// `data`; the dimensions the *model* needs ride in the result's text content,
/// written by the tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultImage {
    /// An API-accepted media type: `image/jpeg`, `image/png`, `image/gif`, or
    /// `image/webp`.
    pub media_type: String,
    /// Standard base64 of the encoded image, stored exactly as it will appear
    /// in the wire block's `source.data`. `#[serde(default)]` because the
    /// control-channel views omit this field (the bytes are fetched from
    /// `GET /log/:seq/images/:idx` instead — `daemon/entry_view.rs`,
    /// docs/control-api.md), and a client parsing a view with this shared type
    /// reads it back empty. The log itself always writes it.
    #[serde(default)]
    pub data: String,
    pub width: u32,
    pub height: u32,
}
