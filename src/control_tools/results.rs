//! The command line's structured results: the `/state` body `state` and `settle` print,
//! the entries `entries` fetches, and an image `image` writes or hands back. The agent
//! verbs answer plaintext instead (docs/tools.md).

use mcplease::traits::ToolOutput;
use mcplease::types::ContentBlock;
use serde::Serialize;
use serde_json::Value;

/// A `/state` snapshot as a tool result (docs/control-api.md).
#[derive(Serialize, schemars::JsonSchema)]
pub struct StateResult {
    /// Present only on a call that waited. `false` means the wait hit its
    /// `timeout_s` and this is where the session was at that moment, not where it
    /// stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) settled: Option<bool>,
    /// The daemon's ephemeral control state: where the loop is (`loop_phase`), the
    /// pending gate if there is one, the pending question (`ask`) if the loop is waiting
    /// on a reply, the inbox, and the running processes.
    pub(super) state: Value,
}

impl StateResult {
    /// A snapshot nobody waited for — no `settled` field to report.
    pub(super) fn plain(body: &str) -> anyhow::Result<Self> {
        Ok(Self {
            settled: None,
            state: serde_json::from_str(body)?,
        })
    }

    /// A snapshot a wait answered with: `settled` says whether it stopped there.
    pub(super) fn waited(body: &str, settled: bool) -> anyhow::Result<Self> {
        Ok(Self {
            settled: Some(settled),
            state: serde_json::from_str(body)?,
        })
    }
}

/// Whole log entries fetched by seq (`GET /log/:seq`, docs/control-api.md).
#[derive(Serialize, schemars::JsonSchema)]
pub struct EntriesResult {
    /// One full entry view per requested seq, in the order asked.
    pub(super) entries: Vec<Value>,
}

// `structured_output!`'s expansion, plus a `to_text` that pretty-prints: the
// program-facing half stays the compact JSON the macro would give, and only the
// command-line rendering is unfolded.
impl ToolOutput for StateResult {
    fn output_schema() -> Option<Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

impl ToolOutput for EntriesResult {
    fn output_schema() -> Option<Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

/// Pretty-printed JSON for the command-line path, falling back to the compact form
/// if it somehow will not render.
pub(super) fn pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value)
        .or_else(|_| serde_json::to_string(value))
        .unwrap_or_default()
}

/// One logged result image, handed back the way the caller can use it: as bytes on
/// disk at a terminal, as an image content block over MCP (docs/control-api.md).
#[derive(Serialize)]
#[serde(untagged)]
pub enum ImageResult {
    /// Written to a file — the command-line path, where there is nothing to render a
    /// content block.
    Wrote { path: String, bytes: usize },
    /// The bytes themselves, base64'd, for an MCP client to render.
    Inline {
        #[serde(skip)]
        data: String,
        mime_type: String,
        bytes: usize,
    },
}

impl ImageResult {
    /// The MCP shape: the bytes base64'd, with the media type read off the bytes
    /// themselves — an image block a client can render.
    pub(super) fn inline(bytes: &[u8], format: Option<image::ImageFormat>) -> Self {
        use base64::Engine as _;
        Self::Inline {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: format
                .map(|f| f.to_mime_type().to_string())
                .unwrap_or_else(|| "application/octet-stream".into()),
            bytes: bytes.len(),
        }
    }
}

impl ToolOutput for ImageResult {
    fn to_content(&self) -> Vec<ContentBlock> {
        match self {
            Self::Wrote { path, bytes } => {
                vec![ContentBlock::text(format!("wrote {path} ({bytes} bytes)"))]
            }
            Self::Inline {
                data, mime_type, ..
            } => vec![ContentBlock::Image {
                data: data.clone(),
                mime_type: mime_type.clone(),
                annotations: None,
                meta: None,
            }],
        }
    }

    fn structured_content(&self) -> Option<Value> {
        None
    }
}
